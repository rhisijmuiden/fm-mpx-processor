# PILOT — huisstijl / brand guide v1.0

**PILOT** is the name of the FM MPX / RDS audio processor. The name refers to the
19 kHz pilot tone: the one signal that makes FM stereo work. The logo mark shows a
sine wave crossed by a single vertical line with a dot on top, which is that pilot.

## Files

| File | Use |
|---|---|
| `tokens.css` | CSS custom properties. Import first in every UI. |
| `tokens.json` | Same tokens for tooling (Tailwind config, charts, scripts). |
| `logo/pilot-mark.svg` | App icon, favicon, compact sidebar. |
| `logo/pilot-wordmark.svg` | Mark + wordmark, login screen, Info dialog, docs. |
| `mockup/index.html` | Interactive design mockup of the web interface (all screens). |

## Character

A dark broadcast control room at night. Deep ink-blue ground, glass panels that
float above a faint measurement grid, and colour used almost only for signal:
meters, the chain, the live state. Everything else stays quiet.

## Colour

| Token | Hex | Role |
|---|---|---|
| `--pl-void` | `#04070E` | Page background |
| `--pl-deep` | `#070C18` | Sidebar, inputs, wells |
| `--pl-surface` | `rgba(20,30,54,.55)` | Glass panels (with 18px backdrop blur) |
| `--pl-text` | `#E8F1FF` | Primary text |
| `--pl-text-2` | `#A9B8D6` | Secondary text |
| `--pl-text-3` | `#6F81A6` | Labels, captions, axis text |
| `--pl-ion` | `#5CE1FF` | **Primary accent.** Active nav, links, focus, mono-band, pilot |
| `--pl-plasma` | `#9D7BFF` | **Secondary accent.** RDS, data, gain reduction |
| `--pl-ok` | `#3DFFB0` | Within limits, connected |
| `--pl-warn` | `#FFB547` | Near limit, update available, test channel |
| `--pl-crit` | `#FF4D6A` | Over limit, ON AIR, hard-limit markers |
| `--pl-left` / `--pl-right` | `#5CE1FF` / `#FF7AC6` | Left and right channel |

Brand gradient: `#5CE1FF → #9D7BFF` at 135°. Use it for the logo, the primary button
and progress bars only.

Meter gradient (bottom to top): cyan → ion → warn at ~85 % → crit at the limit.
Always draw the legal limit (75 kHz deviation, +3 dBr MPX power) as a red marker.

## Typography

| Role | Family | Use |
|---|---|---|
| Display | **Unbounded** 600 | Wordmark, page titles. Never for body text. |
| Body | **Manrope** 400–700 | All interface text. |
| Data | **JetBrains Mono** 400–600 | Every number, frequency, IP, version, RDS text. Tabular figures. |

Panel titles: 13px, uppercase, letter-spacing 0.14em, `--pl-text-2`.
Hero readouts (deviation, LUFS): mono 30–36px.

## Layout rules

- Admin menu always on the **left** (256px), grouped into *On air* and *System*.
  Info & updates button and ON AIR status pinned at the bottom of the menu.
- Top bar: page title, status chips (input, output link, update channel, clock),
  EN/NL language switch.
- Panels: radius 14px, 1px line + subtle gradient edge, 18px padding, 20px gap.
- Below 900px the menu becomes an icon row at the top.

## Components

- **Signal-chain node**: small card per processing stage with live value and gain-reduction bar,
  connected by animated wires. Clicking opens that stage.
- **Arc gauge**: for peak deviation and MPX power, with a red limit tick.
- **Vertical meters**: L/R in channel colours, gain reduction in plasma, hanging from the top.
- **RDS display**: LCD-style panel that shows exactly what a receiver shows.
- **Toggle, segmented control, slider**: ion accent, glow on the active state.
- **Status chip / tag**: mono text in a pill; ok / warn / ion variants.

## Motion

Use motion for live signal only (meters, spectrum, wires, ON AIR pulse).
Page changes: 0.45s rise. Respect `prefers-reduced-motion`.

## Language

English is the default UI language, with a full Dutch translation (EN/NL switch in the top bar).
