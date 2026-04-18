#!/usr/bin/env python3

from __future__ import annotations

import argparse
from collections import deque
from dataclasses import dataclass
from pathlib import Path

from PIL import Image

MAIN_COLUMNS = (1536, 2048, 1024)
MAIN_ROWS = 6
MAIN_ROW_HEIGHT = 1024
ROL_SIZE = (1024, 1024)
SOURCE_DEFAULT = Path.home() / "Pictures" / "WynncraftMapFruma_3.png"
TILES_DIR_DEFAULT = Path(__file__).resolve().parents[1] / "public" / "tiles"
HQ_WEBP_OPTIONS = {"format": "WEBP", "lossless": True, "method": 6}
LQ_WEBP_OPTIONS = {"format": "WEBP", "quality": 80, "method": 6}
LANCZOS = getattr(Image, "Resampling", Image).LANCZOS


@dataclass(frozen=True)
class Component:
    seed: tuple[int, int]
    area: int
    bbox: tuple[int, int, int, int]


def alpha_bbox(image: Image.Image) -> tuple[int, int, int, int]:
    bbox = image.getchannel("A").getbbox()
    if bbox is None:
        raise ValueError("image has no non-transparent pixels")
    return bbox


def union_bbox(boxes: list[tuple[int, int, int, int]]) -> tuple[int, int, int, int]:
    if not boxes:
        raise ValueError("cannot union an empty bbox list")
    min_x = min(box[0] for box in boxes)
    min_y = min(box[1] for box in boxes)
    max_x = max(box[2] for box in boxes)
    max_y = max(box[3] for box in boxes)
    return (min_x, min_y, max_x, max_y)


def find_connected_components(alpha: Image.Image) -> list[Component]:
    width, height = alpha.size
    pixels = alpha.load()
    visited = bytearray(width * height)
    components: list[Component] = []

    for y in range(height):
        for x in range(width):
            idx = y * width + x
            if visited[idx] or pixels[x, y] == 0:
                continue

            seed = (x, y)
            queue = deque([seed])
            visited[idx] = 1
            min_x = max_x = x
            min_y = max_y = y
            area = 0

            while queue:
                cx, cy = queue.popleft()
                area += 1
                min_x = min(min_x, cx)
                max_x = max(max_x, cx)
                min_y = min(min_y, cy)
                max_y = max(max_y, cy)

                for nx, ny in ((cx - 1, cy), (cx + 1, cy), (cx, cy - 1), (cx, cy + 1)):
                    if nx < 0 or ny < 0 or nx >= width or ny >= height:
                        continue
                    nidx = ny * width + nx
                    if visited[nidx] or pixels[nx, ny] == 0:
                        continue
                    visited[nidx] = 1
                    queue.append((nx, ny))

            components.append(Component(seed, area, (min_x, min_y, max_x + 1, max_y + 1)))

    components.sort(key=lambda component: component.area, reverse=True)
    return components


def extract_components(source: Image.Image, seeds: list[tuple[int, int]]) -> Image.Image:
    width, height = source.size
    alpha = source.getchannel("A")
    pixels = alpha.load()
    visited = bytearray(width * height)
    mask = bytearray(width * height)

    for seed_x, seed_y in seeds:
        if pixels[seed_x, seed_y] == 0:
            continue

        seed_idx = seed_y * width + seed_x
        if visited[seed_idx]:
            continue

        queue = deque([(seed_x, seed_y)])
        visited[seed_idx] = 1

        while queue:
            x, y = queue.popleft()
            idx = y * width + x
            mask[idx] = 255

            for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
                if nx < 0 or ny < 0 or nx >= width or ny >= height:
                    continue
                nidx = ny * width + nx
                if visited[nidx] or pixels[nx, ny] == 0:
                    continue
                visited[nidx] = 1
                queue.append((nx, ny))

    mask_image = Image.frombytes("L", source.size, bytes(mask))
    isolated = Image.new("RGBA", source.size, (0, 0, 0, 0))
    isolated.paste(source, (0, 0), mask_image)
    return isolated


def compose_current_main_reference(tiles_dir: Path) -> Image.Image:
    rows: list[Image.Image] = []
    for row in range(1, MAIN_ROWS + 1):
        segments = [
            Image.open(tiles_dir / f"main-{row}-{column}.webp").convert("RGBA")
            for column in range(1, 4)
        ]
        row_width = sum(segment.width for segment in segments)
        row_height = max(segment.height for segment in segments)
        row_image = Image.new("RGBA", (row_width, row_height), (0, 0, 0, 0))

        x = 0
        for segment in segments:
            row_image.paste(segment, (x, 0), segment)
            x += segment.width

        rows.append(row_image)

    canvas_width = max(row.width for row in rows)
    canvas_height = sum(row.height for row in rows)
    canvas = Image.new("RGBA", (canvas_width, canvas_height), (0, 0, 0, 0))

    y = 0
    for row in rows:
        canvas.paste(row, (0, y), row)
        y += row.height

    return canvas


def save_webp(image: Image.Image, path: Path, options: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    image.save(path, **options)


def write_main_tiles(main_canvas: Image.Image, tiles_dir: Path) -> None:
    x_offsets = [0]
    for width in MAIN_COLUMNS[:-1]:
        x_offsets.append(x_offsets[-1] + width)

    for row in range(MAIN_ROWS):
        y1 = row * MAIN_ROW_HEIGHT
        y2 = y1 + MAIN_ROW_HEIGHT

        for column, (x1, width) in enumerate(zip(x_offsets, MAIN_COLUMNS), start=1):
            x2 = x1 + width
            tile = main_canvas.crop((x1, y1, x2, y2))
            tile_path = tiles_dir / f"main-{row + 1}-{column}.webp"
            save_webp(tile, tile_path, HQ_WEBP_OPTIONS)

            lq_tile = tile.resize((width // 2, MAIN_ROW_HEIGHT // 2), LANCZOS)
            save_webp(lq_tile, tiles_dir / "lq" / tile_path.name, LQ_WEBP_OPTIONS)


def write_realm_of_light_tiles(realm_canvas: Image.Image, tiles_dir: Path) -> None:
    save_webp(realm_canvas, tiles_dir / "realm-of-light.webp", HQ_WEBP_OPTIONS)
    save_webp(
        realm_canvas.resize((ROL_SIZE[0] // 2, ROL_SIZE[1] // 2), LANCZOS),
        tiles_dir / "lq" / "realm-of-light.webp",
        LQ_WEBP_OPTIONS,
    )


def write_debug_images(main_canvas: Image.Image, realm_canvas: Image.Image, debug_dir: Path) -> None:
    debug_dir.mkdir(parents=True, exist_ok=True)
    main_canvas.save(debug_dir / "main-preview.png")
    realm_canvas.save(debug_dir / "realm-of-light-preview.png")


def build_main_canvas(source: Image.Image, reference_main: Image.Image) -> Image.Image:
    components = find_connected_components(source.getchannel("A"))
    if not components:
        raise ValueError("source image has no connected alpha components")

    main_component = components[0]
    main_isolated = extract_components(source, [main_component.seed])
    main_crop = main_isolated.crop(main_component.bbox)

    reference_bbox = alpha_bbox(reference_main)
    source_bbox = main_component.bbox
    paste_x = source_bbox[0] + (reference_bbox[2] - source_bbox[2])
    paste_y = source_bbox[1] + (reference_bbox[1] - source_bbox[1])

    canvas = Image.new("RGBA", reference_main.size, (0, 0, 0, 0))
    canvas.paste(main_crop, (paste_x, paste_y), main_crop)
    return canvas


def build_realm_of_light_canvas(source: Image.Image, reference_realm: Image.Image) -> Image.Image:
    components = find_connected_components(source.getchannel("A"))
    if not components:
        raise ValueError("source image has no connected alpha components")

    main_component = components[0]
    realm_components = [
        component
        for component in components[1:]
        if component.bbox[3] <= main_component.bbox[1]
    ]
    if not realm_components:
        raise ValueError("source image has no floating realm-of-light components above the main map")

    realm_isolated = extract_components(source, [component.seed for component in realm_components])
    realm_bbox = union_bbox([component.bbox for component in realm_components])
    realm_crop = realm_isolated.crop(realm_bbox)

    reference_bbox = alpha_bbox(reference_realm)
    canvas = Image.new("RGBA", reference_realm.size, (0, 0, 0, 0))
    canvas.paste(realm_crop, (reference_bbox[0], reference_bbox[1]), realm_crop)
    return canvas


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Refresh Sequoia map background tiles from a full exported PNG."
    )
    parser.add_argument(
        "--source",
        type=Path,
        default=SOURCE_DEFAULT,
        help=f"Source PNG to convert (default: {SOURCE_DEFAULT})",
    )
    parser.add_argument(
        "--tiles-dir",
        type=Path,
        default=TILES_DIR_DEFAULT,
        help=f"Tile output directory (default: {TILES_DIR_DEFAULT})",
    )
    parser.add_argument(
        "--debug-dir",
        type=Path,
        default=None,
        help="Optional directory for PNG previews of the regenerated canvases.",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    source = Image.open(args.source).convert("RGBA")
    reference_main = compose_current_main_reference(args.tiles_dir)
    reference_realm = Image.open(args.tiles_dir / "realm-of-light.webp").convert("RGBA")

    main_canvas = build_main_canvas(source, reference_main)
    realm_canvas = build_realm_of_light_canvas(source, reference_realm)

    write_main_tiles(main_canvas, args.tiles_dir)
    write_realm_of_light_tiles(realm_canvas, args.tiles_dir)

    if args.debug_dir is not None:
        write_debug_images(main_canvas, realm_canvas, args.debug_dir)

    print(f"refreshed map background tiles from {args.source}")
    print(f"wrote HQ/LQ tiles to {args.tiles_dir}")


if __name__ == "__main__":
    main()
