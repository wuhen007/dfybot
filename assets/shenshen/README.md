# Shenshen desktop pet animations

Source: the user's installed Shenshen character from
[MerZlin/dsh-pet-indesktop](https://github.com/MerZlin/dsh-pet-indesktop).
Copyright (c) 2026 Merzlin; the supplied MIT license is preserved in [LICENSE](LICENSE).

| State | Original clip |
| --- | --- |
| Idle | 待机呼吸休闲 |
| Sleeping | 原地小憩沉眠 |
| Thinking / chatting | 工作状态-思考冒泡 |
| Disconnected | 工作状态-垂头叹气冒汗 |
| Happy | 工作状态-雀跃庆祝 |
| Chat disabled / waiting | 东张西望 |

These are selected existing animations, cropped to one shared bounding box and
resized to 216 × 232 RGBA PNG frames at the original 24 fps. Each `meta.json`
preserves the original clip name, source hashes and transformation details.
Only the current animation's compressed frames and one texture are retained.
Python and Pillow are only needed to regenerate assets, never to run the pet:

```powershell
python -m pip install Pillow
python scripts/import_pet_assets.py "path/to/assets/characters/shenshen"
```

The server does not load these assets. When distributing the pet executable,
copy this directory (including the license) to `assets/shenshen` beside it, or
pass `--assets-dir` with an explicit directory.
