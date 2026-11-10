//! Small deterministic noise helpers (tileable value noise, IGN dithering).

#[inline]
pub fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

#[inline]
fn lattice(x: i32, y: i32, period: u32, seed: u32) -> f32 {
    let period = period.max(1) as i32;
    let x = x.rem_euclid(period) as u32;
    let y = y.rem_euclid(period) as u32;
    let h = hash_u32(
        x.wrapping_mul(0x9e37_79b9) ^ y.wrapping_mul(0x85eb_ca6b) ^ seed.wrapping_mul(0xc2b2_ae35),
    );
    (h & 0x00ff_ffff) as f32 / 0x00ff_ffff as f32
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Periodic value noise. `u`,`v` are normally in 0..1.
pub fn tileable_value_noise(u: f32, v: f32, frequency: u32, seed: u32) -> f32 {
    let frequency = frequency.max(1);
    let x = u * frequency as f32;
    let y = v * frequency as f32;
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    let fx = smoothstep(x - x.floor());
    let fy = smoothstep(y - y.floor());
    let a = lattice(xi, yi, frequency, seed);
    let b = lattice(xi + 1, yi, frequency, seed);
    let c = lattice(xi, yi + 1, frequency, seed);
    let d = lattice(xi + 1, yi + 1, frequency, seed);
    (a + (b - a) * fx) + ((c + (d - c) * fx) - (a + (b - a) * fx)) * fy
}

/// Tileable fractal value noise normalized to roughly 0..1.
pub fn tileable_fbm(u: f32, v: f32, frequency: u32, octaves: u32, seed: u32) -> f32 {
    let octaves = octaves.clamp(1, 8);
    let mut sum = 0.0;
    let mut amplitude = 1.0;
    let mut total = 0.0;
    let mut frequency = frequency.max(1);
    for octave in 0..octaves {
        sum += tileable_value_noise(
            u,
            v,
            frequency,
            seed.wrapping_add(octave.wrapping_mul(0x9e37_79b9)),
        ) * amplitude;
        total += amplitude;
        amplitude *= 0.5;
        frequency = frequency.saturating_mul(2);
    }
    if total > 0.0 {
        sum / total
    } else {
        0.0
    }
}

/// Cheap per-pixel rotation used by horizon AO to hide banding.
#[inline]
pub fn interleaved_gradient_noise(x: u32, y: u32) -> f32 {
    let x = x as f32;
    let y = y as f32;
    let value = (x * 0.067_110_56 + y * 0.005_837_15).fract();
    (52.982_918 * value).fract()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_is_deterministic_and_periodic() {
        let a = tileable_fbm(0.13, 0.71, 4, 3, 9);
        let b = tileable_fbm(0.13, 0.71, 4, 3, 9);
        assert_eq!(a, b);
        let left = tileable_value_noise(0.0, 0.37, 8, 4);
        let right = tileable_value_noise(1.0, 0.37, 8, 4);
        assert!((left - right).abs() < 1e-5);
    }

    #[test]
    fn noise_stays_in_unit_range() {
        for y in 0..16 {
            for x in 0..16 {
                let value = tileable_fbm(x as f32 / 16.0, y as f32 / 16.0, 4, 4, 2);
                assert!((0.0..=1.0).contains(&value), "{value}");
            }
        }
    }
}
