//! RNG, sun sampling, and the CPU path-tracing loop.

use glam::Vec3;

use crate::bvh::Bvh;
use crate::mesh::{arbitrary_tangent, normalize_or};

use super::env::EnvSampler;
use super::geometry::{offset_origin, ray_eps};
use super::sampling::{cosine_hemisphere, to_world, Sampler};
use super::{LightmapLightKind, LightmapSettings};

/// Kind tags shared with the GPU shader.
pub(crate) const LIGHT_KIND_POINT: u32 = 0;
pub(crate) const LIGHT_KIND_SPOT: u32 = 1;
pub(crate) const LIGHT_KIND_AREA: u32 = 2;

pub(crate) struct Sun {
    direction: Vec3,
    tangent: Vec3,
    bitangent: Vec3,
    color: Vec3,
    intensity: f32,
    cos_max: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct Rng {
    state: u64,
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

impl Rng {
    /// Seed one texel from the bake seed and the texel index.
    ///
    /// The index must go through a strong mixer: seeding with
    /// `seed * constant + index` makes neighbouring texels use the *same* PCG
    /// stream shifted by one step, which correlates their samples and shows up
    /// as structured noise and banding that barely improves with more spp.
    pub(crate) fn new(seed: u64, pixel: u64) -> Self {
        Self {
            state: splitmix64(seed ^ splitmix64(pixel)),
        }
    }

    fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rotation = (old >> 59) as u32;
        xorshifted.rotate_right(rotation)
    }

    pub(crate) fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 / 16_777_216.0
    }
}
pub(crate) fn create_sun(settings: &LightmapSettings, enabled: bool) -> Sun {
    let size = settings.sun_size_deg.clamp(0.0, 30.0).to_radians();
    let direction = settings.sun_direction();
    let tangent = arbitrary_tangent(direction);
    Sun {
        direction,
        tangent,
        bitangent: direction.cross(tangent),
        color: Vec3::from_array(settings.sun_color),
        intensity: if enabled && settings.sun_enabled {
            settings.sun_intensity.max(0.0)
        } else {
            0.0
        },
        cos_max: size.cos().clamp(-1.0, 1.0),
    }
}

impl Sun {
    fn sample(&self, rng: &mut Rng) -> Vec3 {
        if self.cos_max >= 0.999_999 {
            return self.direction;
        }
        let cos_theta = 1.0 - rng.next_f32() * (1.0 - self.cos_max);
        let sin_theta = (1.0 - cos_theta * cos_theta).max(0.0).sqrt();
        let phi = rng.next_f32() * std::f32::consts::TAU;
        (self.tangent * (sin_theta * phi.cos())
            + self.bitangent * (sin_theta * phi.sin())
            + self.direction * cos_theta)
            .normalize_or_zero()
    }

    /// v2 sample from a Sobol pair, around the sun axis.
    fn sample_with(&self, u: [f32; 2]) -> Vec3 {
        if self.cos_max >= 0.999_999 {
            return self.direction;
        }
        let local = super::sampling::uniform_cone(u[0], u[1], self.cos_max);
        to_world(local, self.direction).normalize_or_zero()
    }
}

/// A scene light converted into tracer-ready form.
///
/// Positions and sizes are already in raw mesh space. The GPU backend packs
/// the same values into its own struct, so both paths see identical inputs.
pub(crate) struct SceneLight {
    pub(crate) kind: u32,
    pub(crate) position: Vec3,
    /// Unit vector from the light toward its target. Also the emitting normal
    /// of an area light.
    pub(crate) direction: Vec3,
    pub(crate) cos_outer: f32,
    pub(crate) cos_inner: f32,
    /// `color * intensity`.
    pub(crate) radiance: Vec3,
    /// Half width and half height of an area light. Zero for delta lights.
    pub(crate) half_size: [f32; 2],
    /// `1.0` for point/spot, `width * height` for area lights.
    pub(crate) area: f32,
    pub(crate) two_sided: bool,
    pub(crate) range: f32,
}

impl SceneLight {
    /// One emitter position. Area lights sample the rectangle uniformly, which
    /// keeps the estimator unbiased; delta lights always return the center.
    fn sample_point(&self, rng: &mut Rng) -> Vec3 {
        if self.kind != LIGHT_KIND_AREA {
            return self.position;
        }
        let tangent = arbitrary_tangent(self.direction);
        let bitangent = self.direction.cross(tangent);
        let u = rng.next_f32() * 2.0 - 1.0;
        let v = rng.next_f32() * 2.0 - 1.0;
        self.position + tangent * (u * self.half_size[0]) + bitangent * (v * self.half_size[1])
    }

    /// v2 area-light sample. All lights share one Sobol pair; the Cranley–
    /// Patterson rotation by the light index gives each a different point set.
    fn sample_point_with(&self, point: [f32; 2], index: usize) -> Vec3 {
        if self.kind != LIGHT_KIND_AREA {
            return self.position;
        }
        let tangent = arbitrary_tangent(self.direction);
        let bitangent = self.direction.cross(tangent);
        let offset = index as f32;
        let u = (point[0] + offset * 0.754_877_7).fract() * 2.0 - 1.0;
        let v = (point[1] + offset * 0.569_840_3).fract() * 2.0 - 1.0;
        self.position + tangent * (u * self.half_size[0]) + bitangent * (v * self.half_size[1])
    }

    /// Smooth cone falloff for spot lights. `cos_light` is the cosine between
    /// the emission axis and the direction toward the shaded point.
    fn cone(&self, cos_light: f32) -> f32 {
        if self.kind != LIGHT_KIND_SPOT {
            return 1.0;
        }
        smoothstep(self.cos_outer, self.cos_inner, cos_light)
    }

    /// Unshadowed weight of one emitter sample, plus the distance and direction
    /// to it. `None` when the sample cannot contribute.
    ///
    /// `weight = cosθ_surface · window / d² · A · cone · emit`, where `emit` is
    /// the Lambertian emitter cosine for area lights and 1 for the delta
    /// emitters (point/spot), which are specified as radiant intensity.
    pub(crate) fn direct_weight(
        &self,
        position: Vec3,
        normal: Vec3,
        light_point: Vec3,
        eps: f32,
        emit_cosine: bool,
    ) -> Option<(f32, f32, Vec3)> {
        let to_light = light_point - position;
        let distance_squared = to_light.length_squared();
        if distance_squared <= eps * eps {
            return None;
        }
        let distance = distance_squared.sqrt();
        let direction = to_light / distance;
        let cosine = normal.dot(direction);
        if cosine <= 0.0 {
            return None;
        }
        // Emission axis versus the direction from the light to the point.
        let cos_light = -self.direction.dot(direction);
        let facing = if self.two_sided {
            cos_light.abs()
        } else {
            cos_light
        };
        if self.kind != LIGHT_KIND_POINT && facing <= 0.0 {
            return None;
        }
        let cone = self.cone(cos_light);
        if cone <= 0.0 {
            return None;
        }
        let window = distance_window(distance, self.range);
        if window <= 0.0 {
            return None;
        }
        let emit = if emit_cosine && self.kind == LIGHT_KIND_AREA {
            facing
        } else {
            1.0
        };
        Some((
            cosine * window / distance_squared * self.area * cone * emit,
            distance,
            direction,
        ))
    }
}

/// Convert the editable lights into tracer-ready form.
///
/// `extent` and `center` come from [`super::prepare_lightmap`] and map the
/// normalized positions stored in the settings into raw mesh space. Disabled
/// lights and lights without intensity are dropped so the trace loop can skip
/// the branch entirely.
pub(crate) fn prepare_scene_lights(
    settings: &LightmapSettings,
    extent: f32,
    center: Vec3,
) -> Vec<SceneLight> {
    if !settings.lighting.uses_lights() {
        return Vec::new();
    }
    let scale = extent.max(1e-5);
    settings
        .lights
        .iter()
        .filter(|light| light.enabled && light.intensity > 0.0)
        .filter_map(|light| {
            let position = Vec3::from_array(light.position) * scale + center;
            let target = Vec3::from_array(light.target) * scale + center;
            if !position.is_finite() || !target.is_finite() {
                return None;
            }
            let direction = (target - position).normalize_or_zero();
            let kind = match light.kind {
                LightmapLightKind::Point => LIGHT_KIND_POINT,
                LightmapLightKind::Spot => LIGHT_KIND_SPOT,
                LightmapLightKind::Area => LIGHT_KIND_AREA,
            };
            // Spot and area lights need a valid aim axis; a point light does
            // not, so its direction may stay zero.
            if kind != LIGHT_KIND_POINT && direction.length_squared() <= 1e-8 {
                return None;
            }
            let width = light.area_size[0].abs() * scale;
            let height = light.area_size[1].abs() * scale;
            let (area, half_size) = if kind == LIGHT_KIND_AREA {
                (width * height, [width * 0.5, height * 0.5])
            } else {
                (1.0, [0.0, 0.0])
            };
            // The spot cone is stored as cosines so the hot loop avoids
            // trigonometry. `inner` is the fully lit part of the cone.
            let outer = light.spot_angle_deg.clamp(0.2, 179.0).to_radians() * 0.5;
            let cos_outer = outer.cos();
            let blend = light.spot_blend.clamp(0.0, 1.0);
            let cos_inner = (outer * (1.0 - blend)).cos();
            Some(SceneLight {
                kind,
                position,
                direction,
                cos_outer,
                cos_inner,
                radiance: Vec3::from_array(light.color).max(Vec3::ZERO) * light.intensity.max(0.0),
                half_size,
                area,
                two_sided: kind == LIGHT_KIND_AREA && light.two_sided,
                range: light.range.max(0.0) * scale,
            })
        })
        .collect()
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let span = edge1 - edge0;
    if span.abs() <= 1e-6 {
        return if value >= edge0 { 1.0 } else { 0.0 };
    }
    let t = ((value - edge0) / span).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Inverse-square falloff with a smooth cutoff at `range`.
///
/// A hard sphere cutoff makes the light visibly end on the surface. The
/// `(1 - t⁴)²` window reaches exactly zero at the range while staying close to
/// physical inverse-square falloff inside it. `range <= 0` means no cutoff.
fn distance_window(distance: f32, range: f32) -> f32 {
    if range <= 0.0 {
        return 1.0;
    }
    let t = (distance / range).clamp(0.0, 1.0);
    let falloff = 1.0 - t * t * t * t;
    falloff * falloff
}
fn cosine_sample(normal: Vec3, rng: &mut Rng) -> (Vec3, f32) {
    let r1 = rng.next_f32();
    let r2 = rng.next_f32();
    let radius = r1.sqrt();
    let phi = r2 * std::f32::consts::TAU;
    let local = Vec3::new(
        radius * phi.cos(),
        radius * phi.sin(),
        (1.0 - r1).max(0.0).sqrt(),
    );
    let tangent = arbitrary_tangent(normal);
    let bitangent = normal.cross(tangent);
    (
        tangent * local.x + bitangent * local.y + normal * local.z,
        local.z.max(0.0),
    )
}

fn balance(first: f32, second: f32) -> f32 {
    let first = first.max(0.0);
    let second = second.max(0.0);
    let a = first * first;
    let b = second * second;
    a / (a + b + 1e-20)
}

/// One cosine-weighted hemisphere visibility sample for an ambient-occlusion
/// bake. White means the ray escaped, black means geometry blocked it. AO uses
/// the whole scene rather than the sun's finite shadow distance so ceilings,
/// walls and neighbouring parts all contribute.
pub(crate) fn trace_ao(
    position: Vec3,
    normal: Vec3,
    bvh: &Bvh,
    eps: f32,
    rng: &mut Rng,
) -> (Vec3, u64, u64) {
    let (direction, _) = cosine_sample(normal, rng);
    let blocked = bvh.occluded(position + normal * eps, direction, f32::INFINITY);
    (
        if blocked { Vec3::ZERO } else { Vec3::ONE },
        1,
        u64::from(blocked),
    )
}

/// Trace one camera path. `ambient` is the solid fill used when no environment
/// is loaded; `fill` is the automatic unshadowed fill added once per path on
/// top of the environment, which keeps surfaces facing away from an HDRI's
/// bright regions lit. Both are unoccluded, matching a large area fill light.
///
/// `lights` are the editable scene lights. Each one gets a next-event
/// estimation sample at every path vertex (sun and environment do the same),
/// so point, spot and area lights cast correct shadows onto the bake.
#[allow(clippy::too_many_arguments)]
pub(crate) fn trace_path(
    mut position: Vec3,
    base_normal: Vec3,
    bvh: &Bvh,
    sampler: Option<&EnvSampler>,
    sun: &Sun,
    lights: &[SceneLight],
    ambient: Vec3,
    fill: Vec3,
    environment_strength: f32,
    bounces: u32,
    eps: f32,
    shadow_distance: f32,
    rng: &mut Rng,
) -> (Vec3, u64, u64) {
    let mut radiance = Vec3::ZERO;
    let mut normal = base_normal;
    let mut offset_normal = normal;
    let mut rays = 0u64;
    let mut hits = 0u64;
    let mut depth = 0u32;

    loop {
        let offset = position + offset_normal * eps;

        if sun.intensity > 0.0 {
            let direction = sun.sample(rng);
            let cosine = normal.dot(direction);
            if cosine > 0.0 {
                rays += 1;
                if bvh.occluded(offset, direction, shadow_distance) {
                    hits += 1;
                } else {
                    radiance += sun.color * sun.intensity * cosine;
                }
            }
        }

        for light in lights {
            let light_point = light.sample_point(rng);
            let Some((weight, distance, to_light)) =
                light.direct_weight(position, normal, light_point, eps, false)
            else {
                continue;
            };
            rays += 1;
            // Stop the shadow ray just short of the emitter so it cannot hit
            // geometry behind the light or self-intersect at the far end.
            let limit = (distance - eps * 2.0).max(eps);
            if bvh.occluded(offset, to_light, limit) {
                hits += 1;
                continue;
            }
            radiance += light.radiance * weight;
        }

        if let Some(sampler) = sampler {
            let (direction, value, pdf_env) = sampler.sample(rng);
            let cosine = normal.dot(direction);
            if cosine > 0.0 {
                rays += 1;
                if bvh.occluded(offset, direction, shadow_distance) {
                    hits += 1;
                } else {
                    let pdf_cos = cosine / std::f32::consts::PI;
                    let weight = balance(pdf_env, pdf_cos);
                    let contribution =
                        value * environment_strength * (cosine / std::f32::consts::PI) / pdf_env
                            * weight;
                    if contribution.is_finite() {
                        radiance += contribution;
                    }
                }
            }
        }

        let (direction, cosine) = cosine_sample(normal, rng);
        rays += 1;
        match bvh.trace(position + offset_normal * eps, direction, f32::INFINITY) {
            Some(hit) => {
                hits += 1;
                if depth >= bounces {
                    break;
                }
                let outward = if hit.face_normal.dot(direction) < 0.0 {
                    hit.face_normal
                } else {
                    -hit.face_normal
                };
                position = hit.point;
                normal = hit.normal;
                if normal.dot(direction) > 0.0 {
                    normal = -normal;
                }
                normal = normalize_or(normal, outward);
                offset_normal = outward;
                depth += 1;
            }
            None => {
                if let Some(sampler) = sampler {
                    let (value, pdf_env) = sampler.eval(direction);
                    let pdf_cos = cosine / std::f32::consts::PI;
                    let weight = balance(pdf_cos, pdf_env);
                    radiance += value * environment_strength * weight;
                } else {
                    radiance += ambient;
                }
                break;
            }
        }
    }

    // Keep the raw HDR path radiance. Clamping the whole path to a fixed
    // value (previously 64) silently darkened legitimately bright HDR light
    // sources and made bakes disagree with Cycles. Isolated fireflies are
    // handled by the built-in denoiser's neighbourhood test; OptiX/OIDN work
    // on HDR input by design. The fill is deterministic and is added after.
    let radiance = radiance + fill;
    (radiance, rays, hits)
}

/// Per-texel running mean and variance of luminance.
///
/// The variance of the sample mean steers the v2 denoiser; it is collected
/// while the path is traced, so no second pass over the samples is needed.
#[derive(Clone, Copy, Default)]
pub(crate) struct Welford {
    n: u32,
    mean: f32,
    m2: f32,
}

impl Welford {
    #[inline]
    pub(crate) fn push(&mut self, value: f32) {
        self.n += 1;
        let delta = value - self.mean;
        self.mean += delta / self.n as f32;
        self.m2 += delta * (value - self.mean);
    }

    /// Variance of the sample mean: `s² / n`.
    #[inline]
    pub(crate) fn var_of_mean(&self) -> f32 {
        if self.n < 2 {
            0.0
        } else {
            self.m2 / (self.n as f32 * (self.n - 1) as f32)
        }
    }
}

#[inline]
pub(crate) fn luminance(color: Vec3) -> f32 {
    0.2126 * color.x + 0.7152 * color.y + 0.0722 * color.z
}

/// Shared inputs of the v2 tracer, so the per-sample function stays readable.
pub(crate) struct TraceContext<'a> {
    pub(crate) bvh: &'a Bvh,
    pub(crate) environment: Option<&'a EnvSampler>,
    pub(crate) sun: &'a Sun,
    pub(crate) lights: &'a [SceneLight],
    pub(crate) ambient: Vec3,
    pub(crate) fill: Vec3,
    pub(crate) environment_strength: f32,
    pub(crate) bounces: u32,
    pub(crate) extent: f32,
    pub(crate) shadow_distance: f32,
    /// Multiply area lights by their Lambertian emitter cosine. Off for the
    /// legacy pipeline so old bakes stay bit-identical.
    pub(crate) area_emitter_cosine: bool,
}

/// v2 path: Owen-Sobol sample order, geometric-normal offsets, chart-aware
/// two-sided bounces and bilinear environment radiance.
pub(crate) fn trace_path_v2(
    sampler: &mut Sampler,
    mut position: Vec3,
    base_normal: Vec3,
    base_geometric: Vec3,
    context: &TraceContext<'_>,
) -> (Vec3, u64, u64) {
    let mut radiance = Vec3::ZERO;
    let mut normal = base_normal;
    let mut offset_normal = base_geometric;
    let mut rays = 0u64;
    let mut hits = 0u64;
    let mut depth = 0u32;

    loop {
        // Fixed pair order: continuation, sun, environment, area lights. Deeper
        // vertices continue drawing past `SOBOL_PAIRS` and fall back.
        let direction_pair = sampler.next_2d();
        let sun_pair = sampler.next_2d();
        let environment_pair = sampler.next_2d();
        let area_pair = sampler.next_2d();

        let eps = ray_eps(context.extent, position);
        let origin = offset_origin(position, offset_normal, eps);

        if context.sun.intensity > 0.0 {
            let direction = context.sun.sample_with(sun_pair);
            let cosine = normal.dot(direction);
            if cosine > 0.0 {
                rays += 1;
                if context
                    .bvh
                    .occluded(origin, direction, context.shadow_distance)
                {
                    hits += 1;
                } else {
                    radiance += context.sun.color * context.sun.intensity * cosine;
                }
            }
        }

        for (index, light) in context.lights.iter().enumerate() {
            let light_point = light.sample_point_with(area_pair, index);
            let Some((weight, distance, to_light)) = light.direct_weight(
                position,
                normal,
                light_point,
                eps,
                context.area_emitter_cosine,
            ) else {
                continue;
            };
            rays += 1;
            let limit = (distance - eps * 2.0).max(eps);
            if context.bvh.occluded(origin, to_light, limit) {
                hits += 1;
                continue;
            }
            radiance += light.radiance * weight;
        }

        if let Some(environment) = context.environment {
            let jitter = [sampler.next_1d(), sampler.next_1d()];
            let (direction, value, pdf_env) = environment.sample_pair(environment_pair, jitter);
            let cosine = normal.dot(direction);
            if cosine > 0.0 {
                rays += 1;
                if context
                    .bvh
                    .occluded(origin, direction, context.shadow_distance)
                {
                    hits += 1;
                } else {
                    let pdf_cos = cosine / std::f32::consts::PI;
                    let weight = balance(pdf_env, pdf_cos);
                    let contribution =
                        value * context.environment_strength * (cosine / std::f32::consts::PI)
                            / pdf_env
                            * weight;
                    if contribution.is_finite() {
                        radiance += contribution;
                    }
                }
            }
        }

        let local = cosine_hemisphere(direction_pair[0], direction_pair[1]);
        let direction = to_world(local, normal);
        let cosine = local.z.max(0.0);
        rays += 1;
        match context.bvh.trace(origin, direction, f32::INFINITY) {
            Some(hit) => {
                hits += 1;
                if depth >= context.bounces {
                    break;
                }
                // Bounce vertices are two-sided: offsets use the face normal on
                // the side the path arrived from, shading uses the interpolated
                // normal flipped to agree with it.
                let geometric = super::geometry::face_normal_toward(hit.face_normal, direction);
                position = hit.point;
                normal = hit.normal;
                if normal.dot(geometric) < 0.0 {
                    normal = -normal;
                }
                normal = normalize_or(normal, geometric);
                offset_normal = geometric;
                depth += 1;
            }
            None => {
                if let Some(environment) = context.environment {
                    let (value, pdf_env) = environment.eval(direction);
                    let pdf_cos = cosine / std::f32::consts::PI;
                    let weight = balance(pdf_cos, pdf_env);
                    radiance += value * context.environment_strength * weight;
                } else {
                    radiance += context.ambient;
                }
                break;
            }
        }
    }

    (radiance + context.fill, rays, hits)
}

/// v2 ambient occlusion: one stratified cosine ray, optionally distance-weighted
/// so a hit contributes `(t / max_t)²` instead of a hard zero.
pub(crate) fn trace_ao_v2(
    sampler: &mut Sampler,
    position: Vec3,
    normal: Vec3,
    geometric: Vec3,
    bvh: &Bvh,
    extent: f32,
    ao_max_distance: f32,
) -> (Vec3, u64, u64) {
    let pair = sampler.next_2d();
    let local = cosine_hemisphere(pair[0], pair[1]);
    let direction = to_world(local, normal);
    let eps = ray_eps(extent, position);
    let origin = offset_origin(position, geometric, eps);
    if ao_max_distance <= 0.0 {
        let blocked = bvh.occluded(origin, direction, f32::INFINITY);
        return (
            if blocked { Vec3::ZERO } else { Vec3::ONE },
            1,
            u64::from(blocked),
        );
    }
    let max_t = (ao_max_distance * extent).max(1e-6);
    match bvh.trace(origin, direction, max_t) {
        None => (Vec3::ONE, 1, 0),
        Some(hit) => {
            let ratio = (hit.t / max_t).clamp(0.0, 1.0);
            (Vec3::splat(ratio * ratio), 1, 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bvh::Bvh;
    use crate::BakeMesh;

    fn floor_and_ceiling() -> BakeMesh {
        BakeMesh {
            positions: vec![
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [1.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
                [-1.0, -1.0, 1.0],
                [1.0, -1.0, 1.0],
                [1.0, 1.0, 1.0],
                [-1.0, 1.0, 1.0],
            ],
            normals: vec![
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, -1.0],
                [0.0, 0.0, -1.0],
                [0.0, 0.0, -1.0],
                [0.0, 0.0, -1.0],
            ],
            uvs: vec![[0.0; 2]; 8],
            indices: vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7],
            ..Default::default()
        }
    }

    #[test]
    fn soft_ao_is_not_quantized_to_sample_count_levels() {
        let mesh = floor_and_ceiling();
        let bvh = Bvh::from_mesh(&mesh).expect("bvh");
        let mut sampler = Sampler::new(3, 0, 0, 64, true);
        let mut values = Vec::new();
        for _ in 0..64 {
            let (value, _, _) = trace_ao_v2(
                &mut sampler,
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::Z,
                Vec3::Z,
                &bvh,
                2.0,
                2.0,
            );
            values.push(value.x);
        }
        assert!(values.iter().all(|value| (0.0..=1.0).contains(value)));
        assert!(
            values.iter().any(|value| *value > 0.01 && *value < 0.99),
            "soft AO only produced binary values: {values:?}"
        );
    }

    #[test]
    fn infinite_ao_stays_binary() {
        let mesh = floor_and_ceiling();
        let bvh = Bvh::from_mesh(&mesh).expect("bvh");
        let mut sampler = Sampler::new(3, 0, 1, 64, true);
        for _ in 0..32 {
            let (value, _, _) = trace_ao_v2(
                &mut sampler,
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::Z,
                Vec3::Z,
                &bvh,
                2.0,
                0.0,
            );
            assert!(value.x == 0.0 || value.x == 1.0);
        }
    }
}
