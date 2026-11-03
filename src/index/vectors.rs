//! The passage embeddings of one book, kept as unit-length rows so that a dot
//! product is a cosine similarity. Row `i` belongs to passage `i`.
//!
//! The file holds the rows back to back as little-endian `f32`; its length is
//! what tells a reader whether it is complete.

use std::fs;
use std::path::Path;

use crate::{Error, Result};

pub struct Vectors {
    rows: usize,
    dim: usize,
    /// `rows * dim` values, row-major.
    data: Vec<f32>,
}

impl Vectors {
    /// Scales every row to unit length; a row of zeros stays zero. All rows
    /// must have the same length.
    pub fn from_rows(rows: Vec<Vec<f32>>) -> Result<Vectors> {
        let count = rows.len();
        let dim = rows.first().map_or(0, Vec::len);
        let mut data = Vec::with_capacity(count * dim);
        for row in &rows {
            if row.len() != dim {
                return Err(Error::Index(format!(
                    "embeddings differ in length: {dim} and {}",
                    row.len()
                )));
            }
            let norm = row.iter().map(|x| x * x).sum::<f32>().sqrt();
            let scale = if norm > 0.0 { norm.recip() } else { 1.0 };
            data.extend(row.iter().map(|x| x * scale));
        }
        Ok(Vectors {
            rows: count,
            dim,
            data,
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let bytes: Vec<u8> = self.data.iter().flat_map(|x| x.to_le_bytes()).collect();
        fs::write(path, bytes)?;
        Ok(())
    }

    /// Reads `rows` rows of `dim` values. A file of any other size is an error.
    pub fn read(path: &Path, dim: usize, rows: usize) -> Result<Vectors> {
        let bytes = fs::read(path)?;
        let expected = rows
            .checked_mul(dim)
            .and_then(|n| n.checked_mul(size_of::<f32>()));
        if expected != Some(bytes.len()) {
            return Err(Error::Index(format!(
                "{} holds {} bytes, not the {rows} rows of {dim} values it should; the cache is incomplete",
                path.display(),
                bytes.len()
            )));
        }
        let data = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect();
        Ok(Vectors { rows, dim, data })
    }

    /// The ids and similarities of the rows nearest to the unit-length `q`:
    /// those scoring at least `floor`, best first and lowest id first among
    /// equals, at most `k`. A `q` of the wrong length has no neighbours.
    pub fn nearest(&self, q: &[f32], k: usize, floor: f32) -> Vec<(u32, f32)> {
        if self.dim == 0 || q.len() != self.dim {
            return Vec::new();
        }
        let mut found: Vec<(u32, f32)> = self
            .data
            .chunks_exact(self.dim)
            .enumerate()
            .map(|(id, row)| {
                (
                    id as u32,
                    row.iter().zip(q).map(|(a, b)| a * b).sum::<f32>(),
                )
            })
            .filter(|&(_, score)| score >= floor)
            .collect();
        found.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        found.truncate(k);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use std::fs;

    fn close(actual: &[f32], expected: &[f32]) -> bool {
        actual.len() == expected.len()
            && actual
                .iter()
                .zip(expected)
                .all(|(a, e)| (a - e).abs() < 1e-6)
    }

    fn rows(vectors: &Vectors) -> Vec<Vec<f32>> {
        vectors
            .data
            .chunks(vectors.dim)
            .map(<[f32]>::to_vec)
            .collect()
    }

    #[test]
    fn vectors_round_trip_unit_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.f32");
        let vectors = Vectors::from_rows(vec![vec![3.0, 4.0], vec![1.0, 0.0]]).unwrap();
        vectors.write(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 4 * 4);

        let read = Vectors::read(&path, 2, 2).unwrap();
        assert_eq!((read.rows(), read.dim()), (2, 2));
        let rows = rows(&read);
        assert!(close(&rows[0], &[0.6, 0.8]), "{rows:?}");
        assert!(close(&rows[1], &[1.0, 0.0]), "{rows:?}");
    }

    #[test]
    fn the_file_is_little_endian_row_major() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.f32");
        Vectors::from_rows(vec![vec![1.0, 0.0], vec![0.0, 2.0]])
            .unwrap()
            .write(&path)
            .unwrap();
        let expected: Vec<u8> = [1.0f32, 0.0, 0.0, 1.0]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        assert_eq!(fs::read(&path).unwrap(), expected);
    }

    #[test]
    fn a_zero_row_stays_zero() {
        let vectors = Vectors::from_rows(vec![vec![0.0, 0.0], vec![0.0, 5.0]]).unwrap();
        assert_eq!(rows(&vectors), [[0.0, 0.0], [0.0, 1.0]]);
    }

    #[test]
    fn rows_of_different_lengths_are_an_error() {
        let error = Vectors::from_rows(vec![vec![1.0, 0.0], vec![1.0]])
            .err()
            .unwrap();
        assert!(matches!(error, Error::Index(_)), "{error}");
    }

    #[test]
    fn no_rows_make_an_empty_set_with_no_neighbours() {
        let vectors = Vectors::from_rows(Vec::new()).unwrap();
        assert_eq!(vectors.rows(), 0);
        assert!(vectors.nearest(&[1.0, 0.0], 5, 0.0).is_empty());
    }

    #[test]
    fn nearest_respects_floor_and_order() {
        let vectors = Vectors::from_rows(vec![vec![0.6, 0.8], vec![1.0, 0.0]]).unwrap();
        let near = vectors.nearest(&[1.0, 0.0], 2, 0.7);
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].0, 1);
        assert!((near[0].1 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn nearest_sorts_by_score_then_id_and_keeps_k() {
        let vectors = Vectors::from_rows(vec![
            vec![0.0, 1.0],
            vec![1.0, 0.0],
            vec![0.6, 0.8],
            vec![1.0, 0.0],
        ])
        .unwrap();
        let ids = |k: usize, floor: f32| -> Vec<u32> {
            vectors
                .nearest(&[1.0, 0.0], k, floor)
                .iter()
                .map(|&(id, _)| id)
                .collect()
        };
        assert_eq!(ids(10, 0.0), [1, 3, 2, 0]);
        assert_eq!(ids(2, 0.0), [1, 3]);
        assert_eq!(ids(10, 0.6), [1, 3, 2]);
        assert!(ids(0, 0.0).is_empty());
    }

    #[test]
    fn a_query_of_the_wrong_length_has_no_neighbours() {
        let vectors = Vectors::from_rows(vec![vec![1.0, 0.0]]).unwrap();
        assert!(vectors.nearest(&[1.0, 0.0, 0.0], 5, 0.0).is_empty());
    }

    #[test]
    fn truncated_vector_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vectors.f32");
        Vectors::from_rows(vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![1.0, 1.0]])
            .unwrap()
            .write(&path)
            .unwrap();
        let bytes = fs::read(&path).unwrap();

        fs::write(&path, &bytes[..bytes.len() - 4]).unwrap();
        assert!(matches!(Vectors::read(&path, 2, 3), Err(Error::Index(_))));
        fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(matches!(Vectors::read(&path, 2, 3), Err(Error::Index(_))));
        let mut longer = bytes.clone();
        longer.extend_from_slice(&[0; 4]);
        fs::write(&path, &longer).unwrap();
        assert!(matches!(Vectors::read(&path, 2, 3), Err(Error::Index(_))));
        fs::write(&path, &bytes).unwrap();
        assert!(Vectors::read(&path, 2, 3).is_ok());
    }

    #[test]
    fn a_missing_vector_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Vectors::read(&dir.path().join("none.f32"), 2, 1).is_err());
    }
}
