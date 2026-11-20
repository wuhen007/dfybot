"""Import selected Shenshen frames: python scripts/import_pet_assets.py CHARACTER_DIR.

Requires Pillow only during import. The Rust client uses the generated PNG files.
"""

import argparse
import json
import shutil
from pathlib import Path

from PIL import Image

CLIPS = {
    "idle": "待机呼吸休闲",
    "sleeping": "原地小憩沉眠",
    "thinking": "工作状态-思考冒泡",
    "offline": "工作状态-垂头叹气冒汗",
    "happy": "工作状态-雀跃庆祝",
    "waiting": "东张西望",
}
# One shared crop keeps the character anchored across every animation.
CROP = (150, 8, 478, 360)
SIZE = (216, 232)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    source = args.source.resolve()
    character = json.loads((source / "character.json").read_text(encoding="utf-8"))
    clips = {clip["name"]: clip for clip in character["clips"]}
    destination = Path(__file__).resolve().parents[1] / "assets" / "shenshen"
    destination.mkdir(parents=True, exist_ok=True)
    for state, name in CLIPS.items():
        clip = clips[name]
        directory = (source / clip["dir"]).resolve()
        if not directory.is_relative_to(source):
            raise ValueError(f"Clip path escapes character directory: {name}")
        meta = json.loads((directory / "meta.json").read_text(encoding="utf-8"))
        frames = sorted(directory.glob("f_*.webp"))
        assert len(frames) == clip["frames"] == meta["frames"], name
        output = destination / state
        output.mkdir(exist_ok=True)
        for index, frame in enumerate(frames, start=1):
            assert frame.name == f"f_{index:04}.webp", frame
            with Image.open(frame) as image:
                assert image.size == tuple(character["canvas"]), frame
                bounds = image.getbbox()
                assert bounds and (
                    CROP[0] <= bounds[0] and CROP[1] <= bounds[1]
                    and CROP[2] >= bounds[2] and CROP[3] >= bounds[3]
                ), f"Crop would clip visible pixels: {frame} {bounds}"
                image.convert("RGBA").crop(CROP).resize(
                    SIZE, Image.Resampling.LANCZOS
                ).save(output / f"f_{index:04}.png", optimize=True)
        expected = {f"f_{index:04}.png" for index in range(1, len(frames) + 1)}
        for stale in output.glob("f_*.png"):
            if stale.name not in expected:
                stale.unlink()
        imported = {
            "fps": meta["fps"],
            "frames": len(frames),
            "size": SIZE,
            "source_clip": clip["dir"],
            "source_meta": meta,
            "crop": CROP,
        }
        (output / "meta.json").write_text(
            json.dumps(imported, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        print(f"{state}: {len(frames)} frames")
    shutil.copyfile(source.parents[2] / "LICENSE", destination / "LICENSE")


if __name__ == "__main__":
    main()
