# Renderer checks

Use this checklist when changing `client/src/gpu`, canvas behavior or shared
label layout. The current canvas enables GPU text during renderer initialization;
there is no separate font-renderer rollout setting. These are manual checks,
not coverage provided by `mise run verify`.

## Visual coverage

Compare the same territory data, timestamp, viewport and settings before and
after the change. Capture the browser/OS, device pixel ratio and zoom level.
Check both the map and claims editor where the behavior is shared.

- Compact tag-only territories at far zoom.
- Tag, name and time layouts on medium and large territories.
- Cooldown urgency colors and time formatting.
- Truncation, abbreviation and long guild or territory names.
- Single, multiple and all-resource icons.
- Hover and selection near labels and icons.
- Live and history views of the same state.
- Claims labels and their layout while panning and zooming.

## Performance

Use a release build for performance comparisons. Record display refresh rate,
frame-time percentiles and the interaction used. Cover continuous panning,
zooming, idle time progression and settings/data changes. Compare with the base
revision on the same machine rather than treating an unmeasured FPS target as
a passed gate.

GPU diagnostics are opt-in: `window.__SEQUOIA_GPU_DIAG__` must be `true` **before**
the renderer initializes. For a local diagnostic build, set it in an early
script in `client/index.html`; do not commit the temporary instrumentation.
The console emits:

```text
gpu-diag static_rebuilds=... dynamic_rebuilds=... icon_rebuilds=... pan_zero_rebuild_frames=...
```

Pan-only frames should usually reuse labels and icons. Time changes should
rebuild dynamic content at content boundaries; static labels should rebuild
for data or settings changes. Counters are reported and reset on rebuilds, so
an interval with no rebuild does not immediately produce a log line.
