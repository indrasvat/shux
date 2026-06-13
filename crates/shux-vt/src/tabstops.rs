/// Mutable horizontal tab-stop state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabStops {
    stops: Vec<bool>,
    extend_defaults: bool,
}

impl TabStops {
    pub fn new(cols: usize) -> Self {
        let mut stops = Vec::with_capacity(cols);
        for col in 0..cols {
            stops.push(Self::default_stop(col));
        }
        Self {
            stops,
            extend_defaults: true,
        }
    }

    pub fn reset(&mut self, cols: usize) {
        *self = Self::new(cols);
    }

    pub fn resize(&mut self, cols: usize) {
        let old_cols = self.stops.len();
        if cols < old_cols {
            self.stops.truncate(cols);
            return;
        }
        for col in old_cols..cols {
            let stop = self.extend_defaults && Self::default_stop(col);
            self.stops.push(stop);
        }
    }

    pub fn set(&mut self, col: usize) {
        if col < self.stops.len() {
            self.stops[col] = true;
        }
    }

    pub fn clear_current(&mut self, col: usize) {
        if col < self.stops.len() {
            self.stops[col] = false;
        }
    }

    pub fn clear_all(&mut self) {
        self.stops.fill(false);
        self.extend_defaults = false;
    }

    pub fn next_from(&self, col: usize, count: usize) -> usize {
        if self.stops.is_empty() {
            return 0;
        }
        let mut current = col.min(self.stops.len() - 1);
        for _ in 0..count.max(1) {
            let Some(next) = self.next_after(current) else {
                return self.stops.len() - 1;
            };
            current = next;
        }
        current
    }

    pub fn prev_from(&self, col: usize, count: usize) -> usize {
        if self.stops.is_empty() {
            return 0;
        }
        let mut current = col.min(self.stops.len() - 1);
        for _ in 0..count.max(1) {
            let Some(prev) = self.prev_before(current) else {
                return 0;
            };
            current = prev;
        }
        current
    }

    fn next_after(&self, col: usize) -> Option<usize> {
        self.stops
            .iter()
            .enumerate()
            .skip(col.saturating_add(1))
            .find_map(|(idx, &is_stop)| is_stop.then_some(idx))
    }

    fn prev_before(&self, col: usize) -> Option<usize> {
        self.stops
            .iter()
            .enumerate()
            .take(col)
            .rev()
            .find_map(|(idx, &is_stop)| is_stop.then_some(idx))
    }

    fn default_stop(col: usize) -> bool {
        col > 0 && col % 8 == 0
    }
}

impl Default for TabStops {
    fn default() -> Self {
        Self::new(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_land_on_eight_column_boundaries() {
        let stops = TabStops::new(40);
        assert_eq!(stops.next_from(0, 1), 8);
        assert_eq!(stops.next_from(8, 1), 16);
        assert_eq!(stops.prev_from(16, 1), 8);
        assert_eq!(stops.prev_from(8, 1), 0);
    }

    #[test]
    fn set_and_clear_preserve_other_default_stops() {
        let mut stops = TabStops::new(40);
        stops.set(12);
        assert_eq!(stops.next_from(0, 1), 8);
        assert_eq!(stops.next_from(8, 1), 12);
        assert_eq!(stops.next_from(12, 1), 16);

        stops.clear_current(8);
        assert_eq!(stops.next_from(0, 1), 12);
        assert_eq!(stops.next_from(12, 1), 16);
    }

    #[test]
    fn clear_all_clamps_and_survives_resize_growth() {
        let mut stops = TabStops::new(16);
        stops.clear_all();
        assert_eq!(stops.next_from(0, 1), 15);
        stops.resize(40);
        assert_eq!(stops.next_from(0, 1), 39);
    }

    #[test]
    fn resize_growth_extends_defaults_after_local_mutations() {
        let mut stops = TabStops::new(16);
        stops.set(12);
        stops.resize(40);
        assert_eq!(stops.next_from(12, 1), 16);
        assert_eq!(stops.next_from(16, 1), 24);
        assert_eq!(stops.next_from(24, 1), 32);
    }

    #[test]
    fn resize_shrink_drops_out_of_range_custom_stops_but_keeps_remaining() {
        let mut stops = TabStops::new(40);
        stops.set(12);
        stops.set(36);

        stops.resize(20);
        assert_eq!(stops.next_from(8, 1), 12);
        assert_eq!(stops.next_from(16, 1), 19);

        stops.resize(40);
        assert_eq!(stops.next_from(32, 1), 39);
    }
}
