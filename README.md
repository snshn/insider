# anatomy

A realtime TUI hardware inspector that draws your mainboard. Ratatui, Linux, 0BSD.

Run it and you get a 2D picture of the machine. The default **semi-exploded**
view shows the mainboard with what has to be plugged in for it to work (CPU,
RAM, M.2 drives, Wi-Fi card) in place, and every peripheral — screen, camera,
microphone, keyboard, touchpad, speakers, battery, fans, USB devices — beside it
with an arrow to the connector or port it's cabled to. Temperatures and live
values sit on each part.

Press `e` for the **exploded** view: RAM, drives and cards also leave their
slots, with their data printed underneath. Soldered parts (laptop CPU/GPU, PCH,
BIOS and any chips the layout has no specific place for) always stay on the
board.

Click any part, or move with Tab/arrows and press Enter, to see everything known
about it: CAS latency and timings for RAM, PCIe link for GPUs/NVMe, battery
voltage/cycles/health, USB speed and drivers, and so on.

## Build & run

    cargo build --release
    ./target/release/anatomy
    sudo ./target/release/anatomy      # adds RAM SPD timings and CPU package power

Optional tools it uses when present: `lspci` (pciutils), `lsusb` (usbutils),
`udevadm` (systemd), `dmidecode`, `nvidia-smi`.

| key | action |
| --- | --- |
| `e` `x` Space | assembled ⇄ exploded |
| click, Tab, arrows/hjkl | select |
| Enter `d` `i` | detail panel |
| PgUp/PgDn, wheel | scroll details |
| `?` | help |
| `q` | quit |

Options: `--board <file>` (try a layout file you're writing), `--list-boards`, `--exploded`, `--interval <ms>`,
`--no-nvidia-smi`, and `--dump`, which prints everything detected plus the
slot assignment as plain text. That dump is the thing to share when a board
needs a proper layout.

### Notes

* **RAM timings** come from the SPD EEPROM: `sudo modprobe ee1004` (DDR4) or
  `spd5118` (DDR5), then run with sudo. Without it you still get size, type,
  speed, part number and slot from SMBIOS (via udev, no root needed).
* **NVIDIA Optimus**: when the dGPU is runtime-suspended (D3) anatomy leaves it
  alone and shows "asleep". When it is awake it polls `nvidia-smi` every ~5 s;
  use `--no-nvidia-smi` if you'd rather never touch it.
* `ANATOMY_SYSROOT=/path` reads `/sys` and `/proc` from a captured tree, and
  `$ANATOMY_SYSROOT/cmd/<tool>` as that tool's recorded output. Handy for
  developing layouts for machines you don't have.

## Board layouts

Layouts are TOML files. anatomy picks one itself: a layout whose `[[match]]`
fits the machine's DMI data wins; otherwise it falls back to a generic stand-in
chosen from what it found (SODIMM/CAMM memory, a battery or a laptop chassis
type mean a laptop board, otherwise ATX). Built-ins: `stock-laptop` and
`stock-atx`. Your own go in `~/.config/anatomy/boards/` (or `./boards/`); a
file with the same id (file stem) as a built-in replaces it.

```toml
name = "Razer Blade 15 (Mid 2019)"
form = "laptop"            # "laptop" or "desktop"
width = 100                # design grid, in terminal cells; scaled to fit
height = 36
pcb = { x = 0, y = 0, w = 100, h = 24 }
color = "#101010"          # PCB colour

[[match]]                  # any [[match]] whose fields all match wins
sys_vendor = "Razer"       # case-insensitive substring of /sys/class/dmi/id/*
product_name = "Blade 15"

[[slot]]
id = "dimm0"
kind = "dimm"              # cpu gpu dimm ssd wifi ethernet chipset bios battery
                           # usb usb-internal fan display camera keyboard
                           # touchpad speaker microphone connector pci pcie
x = 42
y = 12
w = 31
h = 3
label = "SODIMM A"
edge = "bottom"            # side with the gold contacts (dimm, ssd, wifi)
bind = ["ChannelA-DIMM0"]  # optional: only accept the part exposing this key
# link = "edp"             # a peripheral cabled to a connector slot: stays where
                           # it is drawn, with an arrow to that connector
# catchall = true          # soldered area for parts no other slot took
# soldered = true          # stays on the board in exploded view
# multi = true             # takes all remaining parts of its kind
# accepts = ["gpu", "pci"] # override which part kinds fit
```

`bind` keys: DIMM locator / bank locator (SMBIOS), USB root port such as `1-2`,
PCI address such as `0000:01:00.0`, `nvme` / `sata` for drives, interface name
for network cards, `internal`/`eDP`/`HDMI-A` for displays. Without `bind`, parts fill slots of their kind in order.
`anatomy --dump` shows each part's bind keys.

A cell is about twice as tall as it is wide, so draw things about 2× wider than
on a photo.

## License

0BSD
