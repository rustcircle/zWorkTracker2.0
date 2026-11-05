# femtovg-wgpu memory: root cause (baseline before the Vello switch)

## Summary
- The 1.2 GB `phys_footprint` is **GPU-driver-owned memory** ("owned unmapped (graphics)"), not app resources and not a leak.
  Live Metal resources (MTLDevice.currentAllocatedSize) are 39 MB; wgpu texture count stays at 111 for 300 mock seconds.
- Apple's Metal driver (M3 Ultra, macOS 27.2) reserves a per-process render context on the **first render pass** of any
  kind: +484 MB in pure Metal (no wgpu) for one empty clear pass of a 16x16 texture. It is independent of target size,
  queue size, command-buffer kind; blit or compute encoders cost nothing. This is the "~500 MB graphics" of the OpenGL
  build (Apple's GL runs on Metal).
- The driver trims that memory 1.1-1.3 s after the last frame, and the app draws a frame every 1 s while a task runs, so
  it never trims while a task runs (and re-acquires it on the next frame after a trim anyway: 454 MB).
- On top of the floor, the driver adds per-pipeline memory that depends on the **compiled shaders**: a shader that spills
  (256 x vec4 private array) adds +990 MB steady. femtovg's pipeline adds +444 MB over a trivial wgpu pipeline (936 vs
  492 MB steady, even for a frame that only clears); the femtovg fragment uber-shader accounts for only ~34 MB of that
  (trivial fs: 902 MB). Not stencil, varyings, texture sampling, uniform buffers, viewport, draw calls. Remaining suspects
  (not isolated): femtovg's vertex-buffer input layout / vs_main, blend state, write_buffer staging. The real app content
  adds another ~90 MB (1027 MB headless steady).

## Numbers (headless harness, Metal, offscreen 2720x1800 target, release, n = 1-3 per row; spread within 0.5 MB)
| Case | first frame | steady (1 s pacing) |
|---|---|---|
| pure Metal, one clear pass (zz_metal_min) | 488 MB | 488 MB (454 after a trim) |
| pure Metal, two queues | 498 | - |
| pure Metal, blit only / compute only | 4.6 / 4.5 | - |
| wgpu 30, bare pass or trivial draw | 828 | 492 (the extra 336 MB is trimmed ~1.2 s in, even while drawing) |
| wgpu 30, spilling fragment shader | 1818 | 1482 |
| femtovg 0.27 on wgpu, clear only | 946 | 936 |
| femtovg, trivial fs_main | 913 | 903 |
| femtovg, blur filter | 960 | 958 |
| zWorkTracker on femtovg-wgpu (zz_gpu_mem, task running) | 1370 | 1027 (flat 1-40 s real time) |
| real window (REN node, provisional) | - | 1212 at 10 s |

Window size and scale (1360x900 @1x/@2x, 960x600, 1360x1800 @2x) changed nothing (1371 MB each, unpaced).
Steady-state frame: 507 femtovg commands, 37.6k vertices, 3 render-target switches; first frame 145 switches.

## Evidence files
- (scratch, not committed) tests/zz_gpu_mem.rs (headless app on FemtoVGWGPURenderer::render_to_texture, Metal, real-time pacing with ZWT_GPU_REAL=1)
- (scratch, not committed) tests/zz_femtovg_min.rs (femtovg-only and wgpu-only modes, ZWT_MODE, ZWT_WGSL)
- (scratch, not committed) tests/zz_metal_min.rs (pure Metal via objc2-metal)
- (scratch, not committed) vendor/femtovg (instrumented copy: FEMTOVG_LOG_TEX, FEMTOVG_LOG_FRAME, FEMTOVG_NO_DRAW), tex-log.txt, msl-dump.txt, msl-fs.txt
- vmmap-gpu-5s.txt, vmmap-metal.txt, vmmap-wgpu.txt; spill.wgsl, var*.wgsl, tex.wgsl, ubo.wgsl
- cpu-leak-norender.txt, cpu-leak-software.txt (counting allocator, app side with no GPU)

## Consequence
No wgpu setting (frame latency, present mode, power preference, limits, memory hints, instance flags) or asset size
can move this: the driver allocates per process and per compiled pipeline. Levers: a renderer whose pipelines compile
to less driver scratch, fewer frames (none possible: 1 s clock), or no GPU (software renderer: 59 MB).
