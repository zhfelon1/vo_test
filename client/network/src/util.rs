use core::hash::Hash;
use std::{collections::HashMap};
use instant::Instant;

/// used to collect multiple traces and not spam the console
pub(crate) struct DeferredTracer<T: Eq + Hash> {
    _level: log::Level,
    log_enabled: bool, // cache
    items: HashMap<T, u64>,
    last: Instant,
    last_cnt: u32,
}

impl<T: Eq + Hash> DeferredTracer<T> {
    pub(crate) fn new(level: log::Level) -> Self {
        Self {
            _level: level,
            log_enabled: true,
            items: HashMap::new(),
            last: Instant::now(),
            last_cnt: 0,
        }
    }

    pub(crate) fn log(&mut self, t: T) {
        if self.log_enabled {
            *self.items.entry(t).or_default() += 1;
            self.last = Instant::now();
            self.last_cnt += 1;
        } else {
        }
    }

    pub(crate) fn print(&mut self) -> Option<HashMap<T, u64>> {
        const MAX_LOGS: u32 = 10_000;
        const MAX_SECS: u64 = 1;
        if self.log_enabled
            && (self.last_cnt > MAX_LOGS || self.last.elapsed().as_secs() >= MAX_SECS)
        {
            if self.last_cnt > MAX_LOGS {
                log::debug!("this seems to be logged continuously");
            }
            self.last_cnt = 0;
            Some(std::mem::take(&mut self.items))
        } else {
            None
        }
    }
}
