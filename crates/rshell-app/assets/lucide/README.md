# Lucide icons

Source: https://github.com/lucide-icons/lucide/tree/main/icons (retrieved 2026-10-29).
dbx uses the same Lucide family through `@lucide/vue`.

The original 24×24 viewBox, 2px strokes and path geometry are retained. The only SVG change is replacing `currentColor` with white so egui can tint the rendered image for each theme/state. Icons are embedded and rendered by Rust's egui SVG loader; no Web frontend is used. See LICENSE for Lucide's ISC license and the upstream Feather-derived MIT notice. Distribution scripts include this license.

SFTP uses `folder.svg` and `file.svg`, based on the Lucide/Feather folder and file outlines (https://github.com/feathericons/feather/tree/main/icons). These two assets use equivalent compact path commands, 24×24 viewBox and 2px white strokes for theme tinting. The included Feather MIT notice applies.

Session tree additions (2026-11-06): `folder-open.svg`, `chevron-right.svg`, `chevron-down.svg`, and `grip-vertical.svg` retrieved from https://github.com/lucide-icons/lucide/tree/main/icons. Original geometry retained; only `currentColor` replaced with white for theme tinting. The existing Lucide ISC / Feather MIT notices apply.
