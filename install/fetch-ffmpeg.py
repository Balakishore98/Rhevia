"""Fetches the ffmpeg Rhevia carries with it, and packs it for embedding.

Rhevia decodes media files by driving ffmpeg as a subprocess. Everything else
it does is native, so this is the one thing a machine needs that Rhevia
cannot supply itself -- and "install ffmpeg first" is not an answer for
someone who has been handed a single file and told it works.

The build fetched here is LGPL. Rhevia encodes H.264 natively with OpenH264
and only ever asks ffmpeg to decode, so none of the GPL-only encoders are
needed and none are shipped. ffmpeg is invoked as a separate program, never
linked, and is carried unmodified; its licence travels with it.

The two tools are compressed together on purpose. They are built from the
same libraries and are very nearly the same bytes, so a single window wide
enough to span both turns 268 MB into about 44 MB -- the second binary
becomes little more than a reference to the first.

    python install/fetch-ffmpeg.py
"""

import hashlib
import io
import json
import lzma
import pathlib
import shutil
import sys
import urllib.request
import zipfile

# A tagged release rather than master, so a build is repeatable.
RELEASE = "ffmpeg-n8.1-latest-win64-lgpl-8.1"
URL = (
    "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/"
    f"{RELEASE}.zip"
)

# Wide enough to span the first binary, which is what lets the second one
# compress down to almost nothing. The decoder allocates this much, so it is
# no larger than it needs to be.
DICT_SIZE = 256 << 20

ROOT = pathlib.Path(__file__).resolve().parent.parent
VENDOR = ROOT / "desktop" / "vendor" / "ffmpeg"


def progress(label, done, total):
    if total:
        sys.stdout.write(f"\r  {label}: {done / 1e6:6.1f} / {total / 1e6:.1f} MB")
    else:
        sys.stdout.write(f"\r  {label}: {done / 1e6:6.1f} MB")
    sys.stdout.flush()


def download() -> bytes:
    print(f"  from {URL}")
    with urllib.request.urlopen(URL) as response:
        total = int(response.headers.get("Content-Length") or 0)
        chunks = []
        got = 0
        while True:
            chunk = response.read(1 << 20)
            if not chunk:
                break
            chunks.append(chunk)
            got += len(chunk)
            progress("downloading", got, total)
    print()
    return b"".join(chunks)


def main() -> int:
    print("Fetching the ffmpeg Rhevia will carry with it.")
    archive = download()

    with zipfile.ZipFile(io.BytesIO(archive)) as zip_file:
        wanted = {}
        for name in zip_file.namelist():
            tail = name.rsplit("/", 1)[-1]
            # ffplay is a media player with a window of its own. Rhevia never
            # runs it and shipping it would add a third of the size again.
            if tail in ("ffmpeg.exe", "ffprobe.exe"):
                wanted[tail] = zip_file.read(name)
            elif tail in ("LICENSE.txt", "LICENSE"):
                wanted["LICENSE.txt"] = zip_file.read(name)

    missing = {"ffmpeg.exe", "ffprobe.exe"} - wanted.keys()
    if missing:
        print(f"  the archive did not contain {sorted(missing)}", file=sys.stderr)
        return 1

    ffmpeg, ffprobe = wanted["ffmpeg.exe"], wanted["ffprobe.exe"]
    raw = ffmpeg + ffprobe
    print(
        f"  ffmpeg {len(ffmpeg) / 1e6:.1f} MB + "
        f"ffprobe {len(ffprobe) / 1e6:.1f} MB = {len(raw) / 1e6:.1f} MB"
    )

    print("  compressing both together (this takes a couple of minutes)")
    packed = lzma.compress(
        raw,
        format=lzma.FORMAT_XZ,
        filters=[{"id": lzma.FILTER_LZMA2, "preset": 6, "dict_size": DICT_SIZE}],
    )
    print(f"  packed to {len(packed) / 1e6:.1f} MB")

    if VENDOR.exists():
        shutil.rmtree(VENDOR)
    VENDOR.mkdir(parents=True)

    (VENDOR / "tools.xz").write_bytes(packed)
    (VENDOR / "LICENSE.txt").write_bytes(
        wanted.get("LICENSE.txt", b"See https://ffmpeg.org/legal.html\n")
    )
    # Where it came from and what is inside, so the unpacker knows where one
    # binary ends and the next begins, and so anyone can check the bytes.
    (VENDOR / "manifest.json").write_text(
        json.dumps(
            {
                "release": RELEASE,
                "source": URL,
                "licence": "LGPL",
                "ffmpeg_bytes": len(ffmpeg),
                "ffprobe_bytes": len(ffprobe),
                "sha256": hashlib.sha256(raw).hexdigest(),
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )

    print(f"\nReady in {VENDOR}")
    print("Build the single-file version with:")
    print("  cargo build --release -p rhevia-studio --features packed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
