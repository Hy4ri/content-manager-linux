//! Racing line from a layout's `ai/fast_lane.ai` spline.
//!
//! The file starts with four `i32` values (version, point count, lap time, sample
//! count), then one 20-byte record per point: `x, y, z` as `f32`, the distance
//! along the lane as `f32`, and the point index as `i32`. Only the first section is
//! read; the per-point AI data that follows it is not needed for drawing.

use std::fs::File;
use std::io::Read;
use std::path::Path;

const HEADER: usize = 16;
const RECORD: usize = 20;
/// Enough points to draw a smooth line at any card size, cheap to tessellate each frame.
const MAX_POINTS: usize = 600;

/// Racing line seen from above, scaled into the unit square with its aspect ratio kept.
#[derive(Debug, Clone, PartialEq)]
pub struct RacingLine {
    /// Points in `0.0..=1.0` on both axes; `x` is world X, `y` is world Z.
    pub points: Vec<[f32; 2]>,
    /// The lane ends where it starts (a circuit, not a point-to-point stage).
    pub closed: bool,
}

/// Reads the racing line of one layout. `None` when the file is missing, short or empty.
pub fn read(path: &Path) -> Option<RacingLine> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; HEADER];
    f.read_exact(&mut head).ok()?;
    let count = i32::from_le_bytes(head[4..8].try_into().ok()?);
    if !(2..=1_000_000).contains(&count) {
        return None;
    }
    let mut buf = vec![0u8; count as usize * RECORD];
    f.read_exact(&mut buf).ok()?;
    let f32_at = |rec: &[u8; RECORD], at: usize| f32::from_le_bytes(rec[at..at + 4].try_into().unwrap());
    let raw: Vec<[f32; 2]> = buf
        .as_chunks::<RECORD>()
        .0
        .iter()
        .map(|r| [f32_at(r, 0), f32_at(r, 8)])
        .filter(|p| p[0].is_finite() && p[1].is_finite())
        .collect();
    from_world(&raw)
}

/// Downsamples world `(x, z)` points and fits them into the unit square.
pub fn from_world(raw: &[[f32; 2]]) -> Option<RacingLine> {
    if raw.len() < 2 {
        return None;
    }
    let (mut lo, mut hi) = (raw[0], raw[0]);
    for p in raw {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    let size = (hi[0] - lo[0]).max(hi[1] - lo[1]);
    if size <= 0. {
        return None;
    }
    // Centre the shorter axis so the drawing sits in the middle of a square box.
    let off = [(size - (hi[0] - lo[0])) / 2., (size - (hi[1] - lo[1])) / 2.];
    let norm = |p: &[f32; 2]| [(p[0] - lo[0] + off[0]) / size, (p[1] - lo[1] + off[1]) / size];

    let step = raw.len().div_ceil(MAX_POINTS);
    let mut points: Vec<[f32; 2]> = raw.iter().step_by(step).map(norm).collect();
    let last = norm(raw.last().unwrap());
    if points.last() != Some(&last) {
        points.push(last);
    }
    let (a, b) = (points[0], last);
    let gap = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    // A circuit's lane stops a few metres short of its start; a stage ends far away.
    Some(RacingLine { points, closed: gap < 0.05 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_lane(points: &[[f32; 3]]) -> tempfile_path::TempPath {
        let mut bytes = Vec::new();
        for v in [7i32, points.len() as i32, 0, 0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        for (i, p) in points.iter().enumerate() {
            for c in p {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
            bytes.extend_from_slice(&(i as f32).to_le_bytes());
            bytes.extend_from_slice(&(i as i32).to_le_bytes());
        }
        bytes.extend_from_slice(&[0xAB; 64]); // trailing AI data is ignored
        tempfile_path::TempPath::with(&bytes)
    }

    mod tempfile_path {
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicUsize, Ordering};

        pub struct TempPath(pub PathBuf);
        impl TempPath {
            pub fn with(bytes: &[u8]) -> Self {
                static N: AtomicUsize = AtomicUsize::new(0);
                let p = std::env::temp_dir().join(format!(
                    "cm-rs-ai-{}-{}.ai",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::write(&p, bytes).unwrap();
                Self(p)
            }
        }
        impl Drop for TempPath {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
    }

    #[test]
    fn reads_closed_circuit_into_unit_square() {
        let f = write_lane(&[[0., 5., 0.], [100., 5., 0.], [100., 5., 50.], [0., 5., 50.], [0., 5., 1.]]);
        let line = read(&f.0).unwrap();
        assert!(line.closed);
        assert_eq!(line.points.len(), 5);
        // 100 x 50 m box: X spans the square, Z is centred.
        assert_eq!(line.points[0], [0., 0.25]);
        assert_eq!(line.points[1], [1., 0.25]);
        assert_eq!(line.points[2], [1., 0.75]);
    }

    #[test]
    fn point_to_point_stage_is_open() {
        let f = write_lane(&[[0., 0., 0.], [50., 0., 0.], [100., 0., 0.]]);
        assert!(!read(&f.0).unwrap().closed);
    }

    #[test]
    fn short_or_missing_file_is_none() {
        let f = tempfile_path::TempPath::with(&[7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(read(&f.0), None);
        assert_eq!(read(Path::new("/nonexistent/fast_lane.ai")), None);
    }

    #[test]
    fn long_lane_is_downsampled_and_keeps_its_end() {
        let raw: Vec<[f32; 2]> = (0..5000).map(|i| [i as f32, (i as f32 * 0.01).sin()]).collect();
        let line = from_world(&raw).unwrap();
        assert!(line.points.len() <= MAX_POINTS + 1);
        assert_eq!(line.points.last().unwrap()[0], 1.);
    }
}
