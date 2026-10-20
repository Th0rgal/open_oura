//! A small row-major float32 matrix (the feature tables are `[epochs, columns]`).

/// Row-major `rows x cols` float32 matrix; the first column of the feature tables is the epoch
/// timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct Mat {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f32>,
}

impl Mat {
    pub fn new(rows: usize, cols: usize, fill: f32) -> Self {
        Self { rows, cols, data: vec![fill; rows * cols] }
    }

    pub fn from_rows(cols: usize, data: Vec<f32>) -> Self {
        assert!(cols > 0 && data.len() % cols == 0);
        Self { rows: data.len() / cols, cols, data }
    }

    /// Builds a matrix from equally long columns (column stack).
    pub fn from_cols(columns: &[Vec<f32>]) -> Self {
        let rows = columns.first().map_or(0, |c| c.len());
        let mut m = Self::new(rows, columns.len(), 0.0);
        for (j, c) in columns.iter().enumerate() {
            assert_eq!(c.len(), rows, "column {j}");
            for (i, &v) in c.iter().enumerate() {
                m.data[i * m.cols + j] = v;
            }
        }
        m
    }

    pub fn at(&self, i: usize, j: usize) -> f32 {
        self.data[i * self.cols + j]
    }

    pub fn row(&self, i: usize) -> &[f32] {
        &self.data[i * self.cols..(i + 1) * self.cols]
    }

    pub fn col(&self, j: usize) -> Vec<f32> {
        (0..self.rows).map(|i| self.at(i, j)).collect()
    }

    /// Rows for which `keep(row)` holds (boolean-mask indexing).
    pub fn filter_rows(&self, keep: impl Fn(&[f32]) -> bool) -> Mat {
        let mut data = Vec::new();
        for i in 0..self.rows {
            if keep(self.row(i)) {
                data.extend_from_slice(self.row(i));
            }
        }
        Mat { rows: data.len() / self.cols.max(1), cols: self.cols, data }
    }
}
