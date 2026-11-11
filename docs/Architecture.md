# Insider — Architecture

Insider is a realtime TUI hardware inspector for Linux, written in Rust with
Ratatui, licensed 0BSD. It probes the machine, picks a board layout, and draws
a 2D picture of the computer: the mainboard with its chips and slots, and
every peripheral around it with an arrow to the connector it is cabled to.
Everything is clickable for details (temperatures, clocks, RAM timings,
battery health, EDID, USB speed…).

Repository: https://github.com/snshn/insider

This document describes the code as of the rename commit (`1d7b4fb`): what each part does, how data flows, the layout file format, the
drawing rules, and the design decisions behind them.

---

## 1. Design principles

These came from the project owner and should be kept:

1. **Visual first.** Parts are drawn as recognisable shapes, not as labelled
   boxes. Examples: a SODIMM with black DRAM packages, gold edge fingers and a
   key notch; an M.2 SSD with contacts on one end and the mounting half-moon on
   the other; a Wi-Fi card with an RF shield and two antenna sockets; a camera
   as a green strip with a black sensor; braille-drawn fans that spin.
2. **Insider chooses the layout itself.** There is no user-facing layout
   switching. It uses a DMI match if a layout exists for the machine. Otherwise
   it picks a generic stand-in from what it detected: SODIMM or CAMM memory, a
   battery, or a laptop chassis type means a laptop; anything else means ATX.
   The layout name is never shown in the UI.
3. **Two views, no "assembled" view.**
   - **Semi-exploded** (default): parts that must be plugged in for the
     machine to work (CPU, RAM, M.2 drives, Wi-Fi card) sit in their slots.
     Peripherals (screen, camera, microphone, keyboard, touchpad, speakers,
     battery, fans) sit beside the board with an arrow to their connector. USB
     devices always hang off their port with an arrow.
   - **Exploded** (`e`): RAM, drives and cards also leave their slots and are
     drawn in side columns, with their data underneath and an arrow to the
     slot.
   - Soldered parts never move. That covers the laptop CPU/GPU, PCH, BIOS and
     any chip the layout has no specific slot for.
4. **Palette rules.** Gold is reserved for contacts and circuitry. Borders,
   leader lines and arrows are light gray, and turn white when selected or
   active. The board has no decorative texture: a flat PCB colour, a thin
   outline, and the board name (`board_vendor board_name`) in bold white
   bottom-right.
5. **Temperatures go inside each box**, top-right, coloured by value. Chips
   show their label (e.g. "CPU") top-left on the same row.
6. **Linux only for now.** macOS and Windows will need their own probe
   backends later. The UI and layout code is platform-neutral.
7. **The long-term goal is a database** of board, CPU, GPU and RAM layouts, so
   each real machine is drawn in its true shape, colour and arrangement. The
   generic layouts are placeholders until a real layout is contributed, for
   example made from a board photo.

---

## 2. Source map

```
Cargo.toml            ratatui 0.29 (crossterm via ratatui::crossterm), serde, toml 0.8
boards/
  stock-laptop.toml   generic laptop: lid + base peripherals left, board right
  stock-atx.toml      generic ATX desktop board
src/
  main.rs             CLI, startup, probe thread, event loop, --dump
  model.rs            Kind, Part, Core, Snapshot, formatting helpers
  probe.rs            all Linux probing (sysfs/procfs + a few tools)
  board.rs            layout format (serde), loading, choosing, part→slot assignment
  ui.rs               App state, input, every drawing routine, detail panel
```

There are no other crates. All drawing is done cell by cell into Ratatui's
`Buffer`. Ratatui widgets are used only for the header, footer, help popup and
detail panel.

---

## 3. Runtime data flow

```
main()
 ├─ board::load_all()           built-ins + ~/.config/insider/boards + /usr/share/insider/boards + ./boards
 ├─ Prober::new()               one-off: lspci names, SMBIOS memory devices, SPD EEPROMs, notes
 ├─ prober.gather() → Snapshot  first sample (used to choose the layout)
 ├─ board::choose()             --board <file> | best [[match]] | stock-laptop / stock-atx
 ├─ thread: loop { sleep(interval); tx.send(prober.gather()) }     default 1000 ms
 └─ event loop (every 60 ms):
      drain rx → App::set_snapshot (re-runs board::assign)
      terminal.draw(ui::draw_frame)   → scene() + header/footer/details/help
      poll input → on_key / on_click / on_scroll
```

- Probing runs on its own thread, so slow tools (`nvidia-smi`, `wpctl`) never
  block the UI.
- The UI redraws about every 60 ms, even with no new data, so the fans can
  spin smoothly. Only the probe runs on the slower refresh interval.
- `Prober` keeps state between samples for rates: per-CPU `/proc/stat`
  deltas, disk sectors, network bytes, RAPL energy. The first sample has no
  rates.

---

## 4. Data model (`model.rs`)

`Kind` covers both part kinds and slot kinds:
`board cpu gpu dimm ssd wifi ethernet chipset bios battery usb usb-internal fan
pci display camera keyboard touchpad speaker microphone connector pcie`.
`connector` and `pcie` are slot-only kinds.

`Part`, one detected (or expected-but-empty) thing:

| field | meaning |
|---|---|
| `id` | stable key: `cpu0`, `dimm1`, `nvme:nvme0`, `usb:1-2`, `display:card1-eDP-1`, `speaker:l`… |
| `kind`, `title`, `tag` | human name, plus a very short label for small drawings ("16G DDR4", "NVMe 1.0 TB") |
| `present` | false for empty SMBIOS DIMM slots (kept, so slot positions stay right) |
| `temp` | °C |
| `metrics` | short live strings shown on or under the drawing |
| `props` | key/value list for the detail panel |
| `bind` | keys a slot may bind to: DIMM locator, USB root port, PCI address, `nvme`/`sata`, connector type… |
| `level` | 0..1: battery charge, screen brightness, keyboard backlight |
| `rpm` | fans |
| `form` | `SODIMM` / `DIMM` / `CAMM2`… (drives the RAM shape and the laptop guess) |
| `cores` | `Vec<Core { id, temp, ghz, load }>`, one per physical core (CPU) |

`Snapshot { parts, dmi, notes }`. `notes` are human hints, such as "run with
sudo for SPD timings", shown in the board's detail panel.
`Snapshot::is_laptop()` drives the generic layout choice.

---

## 5. Probing (`probe.rs`)

The prober is mostly sysfs/procfs, with no root needed. External tools are
optional and called through `run()`.

| part | sources |
|---|---|
| board | `/sys/class/dmi/id/*`, kernel, hostname, uptime, loadavg, meminfo, thermal zones |
| CPU | `/proc/cpuinfo` (grouped by `physical id`), `cpufreq`, `topology/core_id`, per-CPU `/proc/stat`, hwmon `coretemp`/`k10temp`/`zenpower` ("Core N" labels → per-core temps), RAPL `intel-rapl:0/energy_uj` (root) |
| iGPU | Intel VGA on bus 00 is folded into the CPU part (`gt_act_freq_mhz`) |
| GPU | PCI class 0x03, PCIe link, runtime PM; **if `power/runtime_status` is `suspended`, it is not polled**, so an Optimus dGPU stays asleep; `nvidia-smi` at most every ~5 s when awake (`--no-nvidia-smi` disables it); amdgpu sysfs VRAM/busy |
| RAM | SMBIOS via `udevadm info --path=/sys/devices/virtual/dmi/id` (`MEMORY_DEVICE_*`, no root), falling back to `dmidecode -t 17`; SPD EEPROMs (`ee1004` DDR4, `spd5118` DDR5, root) give tCK/tAA/tRCD/tRP → CL-RCD-RP at the configured speed; `jc42`/`spd5118` hwmon temps; system memory use (% used) shown on every module |
| storage | `/sys/class/nvme/*` (+ namespaces in `/sys/block`, throughput, `nvme` hwmon), SATA `sd*` that are not on USB (`drivetemp`) |
| network | `/sys/class/net/*` with a real device, not USB; Wi-Fi via `wireless/`/`phy80211`, `/proc/net/wireless` signal, rx/tx rates, `iwlwifi` hwmon temp |
| chipset | ISA bridge on bus 00 + `pch_*` hwmon/thermal zone; other unclaimed bus-00 devices listed as integrated functions |
| BIOS | DMI; title "UEFI BIOS <ver>" when `/sys/firmware/efi` exists; Secure Boot from efivars |
| battery | `/sys/class/power_supply` type Battery, scope not Device: charge, voltage, power, Wh/Ah, health, cycles, time to empty/full, AC state |
| fans | every hwmon `fan*_input` |
| displays | `/sys/class/drm/card*-*` with status connected; EDID parsed (vendor PNP id, model strings 0xFC/0xFE, size, native mode + refresh from the first detailed timing); backlight from `/sys/class/backlight`; bind keys = connector type (`eDP`, `HDMI-A`…) and `internal`/`external` |
| keyboard / touchpad | `/proc/bus/input/devices`; keyboard = a node with `kbd` + `leds` handlers that is internal (i8042, I2C, host, or a USB device that is hardwired or whose name contains the DMI `sys_vendor`; Razer's built-in keyboard is USB); touchpad by name or the INPUT_PROP_POINTER bit; Caps/Num/Scroll lock LEDs, `kbd_backlight`; their USB devices are excluded from the USB list |
| speakers / mic | `/proc/asound/card*/codec#*` `[Fixed] Speaker` / `[Fixed] Mic` pins, `pcm*c/info` DMIC; volume via `wpctl get-volume` (PipeWire), else `amixer` |
| USB | `/sys/bus/usb/devices/*` (no hubs); internal vs external from `port/connect_type` (hardwired/hotplug), then `removable`; Video class → camera (`Kind::Camera` when internal); names from sysfs strings, else `lsusb -d`; attached block devices |
| other PCI | anything unclaimed off bus 00 (e.g. a Thunderbolt controller) → `Kind::Pci` |

The `claimed` PCI set stops a device from showing up twice (e.g. the iGPU, the
NVMe controller, the Wi-Fi function).

**Replay mode.** With `INSIDER_SYSROOT=/dir`, every `/sys`/`/proc` path is
read under that directory, and `run(tool)` returns `/dir/cmd/<tool>`. It never
runs the live tool. This makes it possible to develop layouts and probes for
machines you don't have, from a captured tree.

---

## 6. Board layouts (`board.rs`, `boards/*.toml`)

Coordinates are in terminal cells at 1:1. A cell is about twice as tall as it
is wide, so things are drawn about 2× wider than on a photo. The whole
drawing is scaled uniformly to fit the view (max ×1.6 semi-exploded, ×1.4
exploded).

```toml
name = "…"; form = "laptop" | "desktop"; generic = true|false
width = 160; height = 50
pcb = { x, y, w, h }        # the board itself; everything else is laid out around it
color = "#123524"           # PCB colour
[[match]]                   # any table whose fields all match the DMI values (case-insensitive substring)
sys_vendor = "Razer"; product_name = "Blade 15"   # also board_vendor, board_name

[[slot]]
id, kind, x, y, w, h, label
accepts = ["gpu", "pci"]    # default: [kind]; pcie → gpu,pci,ssd,wifi,ethernet; connector → none
soldered = true             # never leaves the board
multi = true                # takes every remaining part of its kinds
bind = ["ChannelA-DIMM0"]   # only parts exposing one of these keys
edge = "left|right|top|bottom"   # side with the contacts (dimm, ssd, wifi)
link = "edp"                # peripheral cabled to that slot (normally a `connector`)
catchall = true             # soldered area for parts no other slot took
```

**Choosing** (`choose`): `--board <file>` (for authoring) → the highest-
scoring `[[match]]` → `stock-laptop` if `Snapshot::is_laptop()` → `stock-atx`.
Files whose id (file stem) matches a built-in replace it. `validate()` checks
kinds, bounds and link targets.

**Assignment** (`assign`), in passes over parts in probe order:
1. slots with `bind`: parts whose `bind` keys intersect;
2. single slots without `bind`, in file order: the next part of an accepted kind;
3. `multi` slots: everything left of their kinds;
4. `catchall` slots: every remaining present part except USB devices and displays.

What is left goes to `Assignment::unplaced`. That only happens when a layout
has no catch-all. Unplaced parts are drawn in a side column without an arrow.

**stock-laptop.toml**, left to right:
- left column: camera and mic above the screen, then the keyboard, the left
  speaker, touchpad and right speaker, then the battery;
- right: two fans above the PCB.

On the PCB:
- connectors `eDP`, `CAM`, `KB`, `TP`, `SPK`, `BAT` and two `FAN` headers,
  which the peripherals `link` to;
- CPU, GPU, PCH, BIOS and the catch-all chip area;
- 2× M.2 SSD, 2× SODIMM, M.2 Wi-Fi, LAN, the internal USB header;
- four USB ports on the right edge.

---

## 7. UI (`ui.rs`)

### State
`App` holds the boards, the chosen index, the snapshot and its `Assignment`,
`exploded`, `details`, `help`, `selected: Option<String>`, `scroll` and
`max_scroll`, and the hit list `hits: Vec<(Rect, key)>`, which is rebuilt every
frame. Keys are a part `id`, `slot:<id>` (an empty or multi slot, or a
connector), or `board`.

### Input
| input | action |
|---|---|
| `e` / `x` / Space | toggle exploded |
| Tab, Shift-Tab | cycle through `hits` in draw order |
| arrows, `hjkl` | spatial navigation: nearest hit in that direction, favouring alignment |
| Enter / `d` / `i` | toggle the detail panel |
| PgUp/PgDn, mouse wheel | scroll details, clamped to `max_scroll` |
| left click | select the topmost hit, which is searched in reverse draw order |
| `?` | help |
| Esc | close help / panel / selection |
| `q`, Ctrl-C | quit |

### Frame (`draw_frame`)
- **Header:** `INSIDER` badge, `sys_vendor product_name`, and a mode badge
  (SEMI-EXPLODED / EXPLODED).
- **Body:** `scene()`.
- **Footer:** key hints, plus a notes counter.
- **Detail panel:** 46 columns wide, opened by Enter or a click. It is only
  scrollable when the wrapped text overflows; `wrapped_rows()` estimates the
  word wrap. The board's panel also lists the notes.

### `scene()`, one function for both views
1. Decide which slots *fly*. That is `kind == usb` always, or in exploded view
   any slot that is not soldered, has no `link` and is not a catch-all.
   Present parts in those slots become items, and unplaced parts are added.
2. Items go to the left or right column by the slot's x relative to the layout
   centre. A column is only allocated if it has items, so the board gets the
   space otherwise.
3. Fit the layout into the middle area (`Xf`: offset + uniform scale) and draw
   the PCB.
4. Stack the items in each column, aligned with their target slot where
   possible.
5. Draw leader lines (item → slot) and cables (slot with `link` → connector)
   *before* the slots, so chips cover the lines and only the arrowheads, drawn
   last, sit on top. `leader()` routes horizontally then vertically. A cable
   drops straight down or up when the connector sits directly under its source.
6. Draw the slots: a catch-all becomes a chip group, a flown slot becomes an
   empty socket or port, and anything else goes through `draw()` with its part.
7. Draw arrowheads, then the flown parts' pictures (`Mode::Pic`) with title,
   temperature and metrics underneath.

### Drawing primitives
`Cv` wraps `&mut Buffer` plus a clip rect. It provides `put`, `text`,
`ctext`, `rtext`, `fill`, `bg`, `frame` (border sets ROUND, PLAIN, HEAVY,
DASH, CHIPB), `restyle`, and `corners` (white corner brackets that mark the
selection on borderless shapes). `aspect()` fits a part's real proportions into
a slot rect, rotating it when the slot is taller than wide. `contacts()` splits
off the contact strip on a given edge.

### Shapes (`draw()` dispatch)
| kind | look |
|---|---|
| CPU | silver heat spreader with a heavy border; title and clock; a grid with one box per physical core showing temp, GHz and load |
| GPU, chipset, BIOS, LAN, PCI, battery | `draw_chip`: label + temperature row, title, metrics; chips get pin marks on the border; the battery gets a charge bar, cell separators and a terminal nub |
| DIMM | `aspect` by form (SODIMM 4.5, DIMM 8.5, CAMM2 ≈3.4 with a compression-connector pad band and screw holes); black DRAM packages, gold `▮` fingers with a key notch |
| M.2 SSD / Wi-Fi | contacts `≡` on the slot edge; SSD: black PCB, NAND packages, half-moon notch; Wi-Fi: green PCB, silver RF shield with text, two `◉` antenna sockets |
| DIMM/M.2 slots | always drawn with a black plastic socket strip on the contact edge (`slot_connector`); empty M.2 slots also show the standoff `◉` |
| connector | black plastic, a row of `╷` pins, label |
| USB port / header | solid metal box with a dark opening; blue plug when something is plugged in. A header has two rows of pins |
| fan | braille rotor (7 swept blades, hub, shroud ring) in a rounded housing; it spins at `min(rpm/4000, 0.9)` rev/s. True RPM would alias at a ~15 fps redraw, so speed only scales with RPM |
| display | bezel + panel whose glow scales with brightness; model, resolution, Hz, size, brightness |
| camera | green strip, black sensor block with a blue lens glint |
| keyboard | keycaps as `▀` (top-half blocks, so the rows have gaps), staggered rows, space bar; Caps Lock LED; brighter with backlight |
| touchpad | rounded pad with the vendor name |
| speaker | grille of dots, volume (red when muted) |
| microphone | row of holes (red when muted) |
| exploded USB device | body + metal plug facing the board (white plug for internal devices) |

Palette constants: `LG` (light gray lines and borders), `HI` (white,
selected), `GOLD` (contacts only), `GREEN_PCB`, `BLACK_PCB`, `METAL`, `SHIELD`,
`CHIP`, `PLASTIC`. Temperature colours: green below 50 °C, yellow below 70 °C,
orange below 85 °C, red above.

---

## 8. CLI

```
insider [-b|--board FILE] [-l|--list-boards] [-d|--dump] [-e|--exploded]
        [-i|--interval MS] [--no-nvidia-smi] [-h] [-V]
```
- `--dump` prints the chosen layout and why, the DMI values, the slot→part
  assignment, and every part with its metrics, bind keys and props. It is the
  thing to ask a user for when making a real layout.
- Running with sudo adds SPD timings and RAPL package power. Under sudo,
  `wpctl` usually can't reach the user's PipeWire session, so audio falls back
  to `amixer`.

---

## 9. Testing

- Unit tests in `probe.rs`: SPD DDR4 → CL, `lspci -mm` parsing and name
  cleanup, udev memory-device parsing, CPU name cleanup, natural sort.
- `board.rs` test: the built-in layouts parse and validate.
- Replay: build a fake tree under `INSIDER_SYSROOT`, with `cmd/lspci`,
  `cmd/udevadm`, `cmd/lsusb`, `cmd/wpctl` and so on, and run `--dump` or the
  TUI against it. Development so far used a fake tree modelled on the Razer
  Blade 15 (2019): i7-9750H, RTX 2060 (runtime-suspended), 2× SODIMM DDR4-2666 with
  SPD, Samsung 970 EVO Plus, Intel 9560 CNVi, ALC298, Sharp 144 Hz eDP panel,
  Razer USB keyboard, Synaptics I2C touchpad, webcam, Bluetooth, a USB receiver
  and a flash drive.
- Visual checks were done by rendering frames to HTML and screenshotting them.
  That needs a headless `Buffer`; a 200×54 frame is a good size to look at.

---

## 10. Known gaps / next steps

- **Real layouts**: none yet. The first should be the owner's Razer Blade 15
  (2019), board vendor Razer, board name **CH530** on the real machine, made
  from `insider --dump` output and a board photo.
- USB ports are filled in order, without knowing which physical port is which;
  real layouts should `bind` ports to root-port keys (`1-1`, `2-1`…). External
  displays have no port slot on the laptop layout (HDMI or mini-DP connectors
  are still to do).
- Fan RPM: the Blade exposes no standard fan hwmon (openrazer is userspace),
  so its fans show "no data". EC or vendor fan drivers could be added.
- Per-DIMM temperatures only appear with `jc42`/`spd5118` sensors; per-module
  RAM *usage* is not knowable, so modules show system-wide use.
- The CAMM2 proportions are approximate.
- The exploded right column can overflow on short terminals ("+N more");
  a second column or scrolling would help.
- macOS/Windows probe backends; a richer layout database (CPU/GPU package
  shapes and colours, chip positions).

---

## 11. Working conventions

- Changes are delivered as patch files, one per commit, named
  `insider-NNNN-short-feature-name.patch` and numbered in apply order. They
  are applied with `git apply`, not `git am`. The code is moved between
  machines by USB drive, so a patch, or a full source tarball, is the unit of
  delivery.
- The test machine is the 2019 Razer Blade 15 running Ubuntu 24/26 under
  Wayland, with Intel + NVIDIA Optimus.
- The license stays 0BSD.
