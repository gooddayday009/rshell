# Lucide icons

Source: https://github.com/lucide-icons/lucide/tree/main/icons (retrieved 2026-10-29).
dbx uses the same Lucide family through `@lucide/vue`.

The original 24×24 viewBox, 2px strokes and path geometry are retained. The only SVG change is replacing `currentColor` with white so egui can tint the rendered image for each theme/state. Icons are embedded and rendered by Rust's egui SVG loader; no Web frontend is used. See LICENSE for Lucide's ISC license and the upstream Feather-derived MIT notice. Distribution scripts include this license.
