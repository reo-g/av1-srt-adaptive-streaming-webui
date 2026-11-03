# design

- `screens/` — the approved designs (Claude Design canvas, page「改訂案」), rendered as images.
  01–02 operation (tx/rx), 03–04 video settings (auto / editing manual), 05 transmission settings,
  06–08 full screen (tx/rx/spec), 09 the colour palette that was chosen (案1, black accent).
- `canvas/` — sources of those boards. They use the Design canvas runtime and do not run on their
  own; read them for exact wording, spacing and state logic (the `renderVals` scripts).
