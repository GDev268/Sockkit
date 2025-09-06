use std::collections::VecDeque;

pub(crate) struct BackpressureBuffer<T> {
    buffer: VecDeque<T>,
    max_capacity: usize,
}

impl<T> BackpressureBuffer<T> {
    pub(crate) fn new(max_capacity: usize) -> BackpressureBuffer<T> {
        BackpressureBuffer {
            buffer: VecDeque::with_capacity(max_capacity),
            max_capacity,
        }
    }

    pub(crate) fn push(&mut self, value: T) {
        if self.buffer.len() < self.max_capacity {
            self.buffer.push_back(value);
        } else {
            let dropped = self.buffer.pop_front();
            self.buffer.push_back(value);

            drop(dropped)
        }
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        self.buffer.pop_front()
    }

    pub(crate) fn flush(&mut self) -> impl Iterator<Item = T> {
        self.buffer.drain(..)
    }
}