#[derive(Clone)]
pub(in crate::fuse) struct ShapeRecovery {
    failures: u32,
    window_start: Option<std::time::Instant>,
    last_forced: Option<std::time::Instant>,
}

impl ShapeRecovery {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(10);
    const COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5);
    const THRESHOLD: u32 = 8;

    pub(super) fn new() -> Self {
        Self {
            failures: 0,
            window_start: None,
            last_forced: None,
        }
    }

    pub(super) fn note(&mut self) -> bool {
        self.note_at(std::time::Instant::now())
    }

    pub(super) fn note_at(&mut self, now: std::time::Instant) -> bool {
        match self.window_start {
            Some(start) if now.duration_since(start) <= Self::WINDOW => {
                self.failures += 1;
            }
            _ => {
                self.window_start = Some(now);
                self.failures = 1;
            }
        }
        if self.failures < Self::THRESHOLD {
            return false;
        }
        if let Some(last) = self.last_forced {
            if now.duration_since(last) < Self::COOLDOWN {
                return false;
            }
        }
        self.last_forced = Some(now);
        self.failures = 0;
        self.window_start = None;
        true
    }
}
