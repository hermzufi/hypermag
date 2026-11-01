//! Piece picker (pure state machine).

#![forbid(unsafe_code)]

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PieceState {
    Missing,
    Pending,
    Have,
}

#[derive(Debug, Clone)]
pub struct Picker {
    pub piece_count: u32,
    pub state: Vec<PieceState>,
    pub availability: Vec<u32>,
}

impl Picker {
    #[must_use]
    pub fn new(piece_count: u32) -> Self {
        Self {
            piece_count,
            state: vec![PieceState::Missing; piece_count as usize],
            availability: vec![0u32; piece_count as usize],
        }
    }

    pub fn update_availability(&mut self, piece: u32, have: bool) {
        if piece >= self.piece_count {
            return;
        }
        let idx = piece as usize;
        if have {
            self.availability[idx] = self.availability[idx].saturating_add(1);
        } else {
            self.availability[idx] = self.availability[idx].saturating_sub(1);
        }
    }

    pub fn mark_have(&mut self, piece: u32) {
        if piece < self.piece_count {
            self.state[piece as usize] = PieceState::Have;
        }
    }

    /// Rarest-first piece index to download.
    pub fn next_piece(&self) -> Option<u32> {
        let mut best: Option<(u32, u32)> = None; // (avail, index)
        for i in 0..self.piece_count {
            if self.state[i as usize] == PieceState::Have {
                continue;
            }
            let a = self.availability[i as usize];
            match best {
                None => best = Some((a, i)),
                Some((ba, _bi)) if a < ba => best = Some((a, i)),
                _ => {}
            }
        }
        best.map(|(_, i)| i)
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.state.iter().all(|s| matches!(s, PieceState::Have))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rarest_first() {
        let mut p = Picker::new(3);
        p.update_availability(0, true);
        p.update_availability(0, true);
        p.update_availability(1, true);
        p.update_availability(2, true);
        p.update_availability(2, true);
        p.update_availability(2, true);
        assert_eq!(p.next_piece(), Some(1));
    }

    #[test]
    fn skip_have() {
        let mut p = Picker::new(2);
        p.mark_have(0);
        p.update_availability(1, true);
        assert_eq!(p.next_piece(), Some(1));
    }
}
