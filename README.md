# Color Bench

A Vulkan layer that draws color analysis tools on top of any application's
swapchain: histograms, waveform parades and fullscreen false-color overlays.
SDR and HDR10.

This is an early release, expect bugs, rough edges and missing features.

## Features

- Luminance and RGB histograms
- Luminance and RGB parades
- Overlays:
  - **Clipping**: any channel at or above 1.0 in red, pixels at or below 0.0 in blue
  - **Channels**: a single R, G, B or A channel in grayscale
  - **Luminance** (HDR10 only): false color on a PQ scale from 0 to 10,000 nits
  - **Gamut** (HDR10 only): smallest containing gamut, red for Rec. 709,
    green for P3, blue for Rec. 2020

Analysis runs on a downscaled copy of the frame, read back asynchronously and
processed on a worker thread. The present thread never waits on it.

## Build

You need `glslc` from the Vulkan SDK, found through `VULKAN_SDK` or `PATH`.

```powershell
cargo xtask package --release
```

This produces `VkLayer_bc_color_bench.dll` and `VK_LAYER_BC_Color_Bench.json`
side by side in `target/release`.

## Use

Color Bench is an explicit layer. Point the loader at the output directory and
enable it by name:

```powershell
$env:VK_ADD_LAYER_PATH = "C:\path\to\vk-color-bench\target\release"
$env:VK_INSTANCE_LAYERS = "VK_LAYER_BC_Color_Bench"
.\your-app.exe
```

A "Color Bench" window appears in the top-left corner. While the pointer is
over it, mouse and keyboard input is kept from the application.

## Settings

Environment variables, read when the device or swapchain is created:

| Variable | Default | Meaning |
| --- | --- | --- |
| `VK_COLOR_BENCH_ANALYSIS_HZ` | `12` | Capture rate, clamped to 1-240 |
| `VK_COLOR_BENCH_MAX_WIDTH` | half the swapchain width | Width of the analysed image; `0` keeps full resolution |
| `VK_COLOR_BENCH_TRANSFER_QUEUE` | `1` | `0` stops the layer from reserving its own transfer queue |
| `RUST_LOG` | `warn` | Log level (`info` and `debug` are useful when something is off) |

## Supported targets

- **Platform**: Windows. Linux builds but is untested.
- **Swapchain formats for analysis**: `R8G8B8A8` and `B8G8R8A8` (`UNORM` and
  `SRGB`), `A2R10G10B10_UNORM` and `A2B10G10R10_UNORM`.
- **HDR overlays**: `HDR10_ST2084` color space only.
