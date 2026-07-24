use std::collections::HashMap;

pub const TILE_RES: u32 = 128;
pub const TILE_SIZE: f32 = 128.0;
pub const GRID_TILES: u32 = 4;
pub const CANVAS_SIZE: u32 = GRID_TILES * TILE_RES;

#[derive(Debug, Clone)]
pub struct SDFTile {
    pub data: Vec<f32>,
    pub dirty: bool,
}

impl SDFTile {
    pub fn new(initial_value: f32) -> Self {
        Self {
            data: vec![initial_value; TILE_RES as usize * TILE_RES as usize],
            dirty: false,
        }
    }
}

pub struct SparseSDFGrid {
    pub tiles: HashMap<(i32, i32), SDFTile>,
}

impl SparseSDFGrid {
    pub fn new() -> Self {
        Self {
            tiles: HashMap::new(),
        }
    }

    pub fn tile_key(world_x: f32, world_y: f32) -> (i32, i32) {
        (
            (world_x / TILE_SIZE).floor() as i32,
            (world_y / TILE_SIZE).floor() as i32,
        )
    }

    pub fn get_or_create(&mut self, key: (i32, i32)) -> &mut SDFTile {
        self.tiles
            .entry(key)
            .or_insert_with(|| SDFTile::new(TILE_SIZE))
    }

    pub fn stamp_circle(&mut self, cx: f32, cy: f32, radius: f32) {
        let min_key = Self::tile_key(cx - radius, cy - radius);
        let max_key = Self::tile_key(cx + radius, cy + radius);

        for ty in min_key.1..=max_key.1 {
            for tx in min_key.0..=max_key.0 {
                let tile = self.get_or_create((tx, ty));
                let ox = tx as f32 * TILE_SIZE;
                let oy = ty as f32 * TILE_SIZE;

                for py in 0..TILE_RES {
                    let wy = oy + (py as f32 + 0.5) / TILE_RES as f32 * TILE_SIZE;
                    for px in 0..TILE_RES {
                        let wx = ox + (px as f32 + 0.5) / TILE_RES as f32 * TILE_SIZE;
                        let d = ((wx - cx).powi(2) + (wy - cy).powi(2)).sqrt() - radius;
                        let idx = (py * TILE_RES + px) as usize;
                        let old = tile.data[idx];
                        tile.data[idx] = old.min(d);
                    }
                }
                tile.dirty = true;
            }
        }
    }

    pub fn clear_dirty(&mut self) {
        for tile in self.tiles.values_mut() {
            tile.dirty = false;
        }
    }
}
