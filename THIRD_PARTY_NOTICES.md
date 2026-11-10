# Third-party notices

## Intel Open Image Denoise (OIDN)

The portable Windows build ships the Intel Open Image Denoise runtime
(`OpenImageDenoise*.dll`, `tbb12.dll`, `tbbbind*.dll`) next to `N-shade.exe`.

- Project: <https://github.com/OpenImageDenoise/oidn>
- Version: 2.3.1
- Copyright: 2018-2024 Intel Corporation
- License: Apache License 2.0

The full license text is included in the portable archive as
`OIDN-LICENSE.txt`; the oneTBB, oneDNN and DPCPP notices are included as
`OIDN-THIRD-PARTY-PROGRAMS.txt`.

NVIDIA OptiX is not redistributed: the app loads `nvoptix.dll` from the
installed NVIDIA driver at runtime.
