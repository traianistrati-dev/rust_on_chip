#!/usr/bin/env python3
"""Harvest every STM32 part's flash geometry from stm32-metapac.

The Configuration tab's flash store keeps settings in the chip's own flash and
keeps them out of memory.x's FLASH region. To do it the IDE needs, per part,
facts its chip definitions do not carry: the flash size, the erase unit at the
END of flash and at its START, and whether the flash is a plain row of equal
pages that read 0xFF when erased. A rule per family is not enough - measured
here: F0 has 1 KiB and 2 KiB parts at the same size (F071x8), the F1 page
follows the density, G4 has 2 KiB and 4 KiB lines, H7 128 KiB and 8 KiB, and
L0/L1 erase to 0x00.

This reads the same metadata embassy-stm32's build script reads (it derives its
`FLASH_SIZE`, `MAX_ERASE_SIZE`, `WRITE_SIZE` and `BANK1_REGION1` from it), so
the IDE and the firmware cannot disagree. The generated code checks that again
at compile time.

Per part, from the FIRST memory configuration and the `BANK_*` flash regions
(OTP excluded):

    size       total bytes of the BANK regions (= embassy's FLASH_SIZE)
    page       erase size of the region at the end of flash
    write      write size (embassy's WRITE_SIZE; sequential-storage's word)
    head_page  erase size of the region at the START of flash (F2/F4/F7: the
               small sectors the store can use after the vector table)
    head_size  that first region's size (embassy's BANK1_REGION1.size)
    flags      UNIFORM (one erase size), CONTIGUOUS (no gap between banks),
               ERASED_FF (erased bytes read 0xFF), CONFIGURABLE (more than one
               memory configuration: single/dual bank chosen in option bytes)

    python scripts/harvest-flash-geometry.py [path-to-stm32-metapac]
"""

import glob
import os
import re
import sys

OUT = os.path.join("src", "panels", "mcu_module", "stm32_flash_geometry.rs")

REGION = re.compile(
    r'MemoryRegion\s*\{\s*name:\s*"([^"]+)",\s*kind:\s*MemoryRegionKind::(\w+),\s*'
    r'address:\s*(0x[0-9a-fA-F]+|\d+),\s*size:\s*(\d+),\s*settings:\s*(?:None|Some\(FlashSettings\s*\{\s*'
    r'erase_size:\s*(\d+),\s*write_size:\s*(\d+),\s*erase_value:\s*(\d+),?\s*\}\))',
    re.S,
)

UNIFORM, CONTIGUOUS, ERASED_FF, CONFIGURABLE = 1, 2, 4, 8


def find_metapac(argv):
    if len(argv) > 1:
        return argv[1]
    home = os.environ.get("USERPROFILE") or os.environ.get("HOME") or ""
    hits = sorted(glob.glob(os.path.join(home, ".cargo", "registry", "src", "*", "stm32-metapac-*")))
    if not hits:
        sys.exit("stm32-metapac not found in the cargo registry; pass its path")
    return hits[-1]


def configurations(text):
    """The top-level `&[ ... ]` groups of `memory: &[ ... ]`."""
    mem = text[text.index("memory:"): text.index("peripherals:")]
    out, depth, start, i = [], 0, None, mem.index("&[") + 2
    while i < len(mem):
        if mem.startswith("&[", i):
            depth += 1
            if depth == 1:
                start = i
            i += 2
            continue
        if mem[i] == "]":
            if depth == 1:
                out.append(mem[start:i])
            depth -= 1
            if depth < 0:
                break
        i += 1
    return out


def geometry(path):
    text = open(path, encoding="utf-8").read()
    name = re.search(r'name:\s*"([^"]+)"', text).group(1).lower()
    configs = configurations(text)
    regions = []
    for m in REGION.finditer(configs[0]):
        if m.group(2) != "Flash" or not m.group(1).startswith("BANK") or not m.group(5):
            continue
        regions.append((int(m.group(3), 0), int(m.group(4)), int(m.group(5)), int(m.group(6)), int(m.group(7))))
    if not regions:
        return name, None
    regions.sort()
    flags = 0
    if len({r[2] for r in regions}) == 1:
        flags |= UNIFORM
    if all(regions[i][0] + regions[i][1] == regions[i + 1][0] for i in range(len(regions) - 1)):
        flags |= CONTIGUOUS
    if {r[4] for r in regions} == {255}:
        flags |= ERASED_FF
    if len(configs) > 1:
        flags |= CONFIGURABLE
    size = sum(r[1] for r in regions)
    shape = (regions[-1][2], max(r[3] for r in regions), flags, regions[0][2], regions[0][1])
    return name, (size, shape)


def main():
    root = find_metapac(sys.argv)
    chips_dir = os.path.join(root, "src", "chips")
    parts = {}
    for d in sorted(os.listdir(chips_dir)):
        p = os.path.join(chips_dir, d, "metadata.rs")
        if not os.path.isfile(p):
            continue
        name, geo = geometry(p)
        if geo is None:
            continue  # N6: no internal flash
        if name in parts and parts[name] != geo:
            sys.exit(f"{name}: two cores disagree on the flash ({parts[name]} vs {geo})")
        parts[name] = geo

    shapes = sorted({g[1] for g in parts.values()})
    index = {s: i for i, s in enumerate(shapes)}
    version = os.path.basename(root.rstrip("\\/"))

    lines = [
        "//! Flash geometry of every STM32 part, harvested from stm32-metapac.",
        "//!",
        "//! GENERATED by `scripts/harvest-flash-geometry.py` — do not edit by hand.",
        f"//! Source: {version} — the metadata embassy-stm32 0.6 derives its",
        "//! `FLASH_SIZE`, `MAX_ERASE_SIZE`, `WRITE_SIZE` and `BANK1_REGION1` from.",
        "//!",
        "//! `size` is the total of the `BANK_*` regions (OTP excluded), `page` the",
        "//! erase size of the region at the END of flash, `write` the write size,",
        "//! `head_page` / `head_size` the first region's erase size and size.",
        "//! Read through [`super::flash_store`]; see the script for why a rule per",
        "//! family would not do.",
        "",
        "/// One flash layout, shared by every part that has it (sizes aside).",
        "#[derive(Clone, Copy, Debug, PartialEq, Eq)]",
        "pub struct Shape {",
        "    pub page: u32,",
        "    pub write: u8,",
        "    pub flags: u8,",
        "    pub head_page: u32,",
        "    pub head_size: u32,",
        "}",
        "",
        "/// Every erase unit is the same size.",
        f"pub const UNIFORM: u8 = {UNIFORM};",
        "/// The banks follow each other with no gap in between.",
        f"pub const CONTIGUOUS: u8 = {CONTIGUOUS};",
        "/// Erased bytes read 0xFF (L0/L1 read 0x00).",
        f"pub const ERASED_FF: u8 = {ERASED_FF};",
        "/// Single or dual bank is chosen in the option bytes (two memory",
        "/// configurations in metapac); the page size follows that choice.",
        f"pub const CONFIGURABLE: u8 = {CONFIGURABLE};",
        "",
        f"pub static SHAPES: [Shape; {len(shapes)}] = [",
    ]
    # rustfmt's own layout, so `cargo fmt` leaves a fresh table alone.
    for page, write, flags, head_page, head_size in shapes:
        lines += [
            "    Shape {",
            f"        page: {page},",
            f"        write: {write},",
            f"        flags: {flags},",
            f"        head_page: {head_page},",
            f"        head_size: {head_size},",
            "    },",
        ]
    lines += [
        "];",
        "",
        "/// (part, flash size in KiB, index into [`SHAPES`]), sorted by part - the",
        "/// part as embassy-stm32 names its feature (`stm32g431cb`).",
        f"pub static PARTS: [(&str, u16, u8); {len(parts)}] = [",
    ]
    for name in sorted(parts):
        size, shape = parts[name]
        assert size % 1024 == 0, name
        lines.append(f'    ("{name}", {size // 1024}, {index[shape]}),')
    lines += ["];", ""]
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines))
    print(f"wrote {OUT}: {len(parts)} parts, {len(shapes)} shapes")


if __name__ == "__main__":
    main()
