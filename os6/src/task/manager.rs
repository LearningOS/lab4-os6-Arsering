//! Implementation of [`TaskManager`]
//!
//! It is only used to manage processes and schedule process based on ready queue.
//! Other CPU process monitoring functions are in Processor.

use super::task::Schedule;
use super::TaskControlBlock;
use crate::sync::UPSafeCell;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::*;

pub struct TaskManager {
    ready_queue: VecDeque<Arc<TaskControlBlock>>,
}

// YOUR JOB: FIFO->Stride
/// A simple FIFO scheduler.
impl TaskManager {
    pub fn new() -> Self {
        Self {
            ready_queue: VecDeque::new(),
        }
    }
    /// Add process back to ready queue
    pub fn add(&mut self, task: Arc<TaskControlBlock>) {
        self.ready_queue.push_back(task);
    }
    /// Take a process out of the ready queue
    pub fn fetch(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.ready_queue.pop_front()
    }

    /// Take a process out of the ready queue
    pub fn stride_scheduling(&mut self) -> Option<Arc<TaskControlBlock>> {
        if self.ready_queue.len() == 0 {
            return None;
        }
        let mut result_id = (0..self.ready_queue.len())
            .min_by_key(|id| self.ready_queue[*id].inner_exclusive_access().schedule.pass);

        if self.ready_queue[result_id.unwrap()]
            .inner_exclusive_access()
            .schedule
            .pass
            == usize::MAX
        {
            for item in self.ready_queue.iter_mut() {
                let schedule_tmp = &mut item.inner_exclusive_access().schedule;
                schedule_tmp.update_pass(false);
            }
            // 重新选取即将在CPU中运行的task
            result_id = (0..self.ready_queue.len())
                .min_by_key(|id| self.ready_queue[*id].inner_exclusive_access().schedule.pass);
        }

        let mut result = self.ready_queue.remove(result_id.unwrap());
        {
            let schedule_tmp = &mut result.as_mut().unwrap().inner_exclusive_access().schedule;
            schedule_tmp.update_pass(true);
        }
        result
    }
}

lazy_static! {
    /// TASK_MANAGER instance through lazy_static!
    pub static ref TASK_MANAGER: UPSafeCell<TaskManager> =
        unsafe { UPSafeCell::new(TaskManager::new()) };
}

pub fn add_task(task: Arc<TaskControlBlock>) {
    TASK_MANAGER.exclusive_access().add(task);
}

pub fn fetch_task() -> Option<Arc<TaskControlBlock>> {
    TASK_MANAGER.exclusive_access().fetch()
}

/// 根据stride scheduling从TaskManager中pop出一个task
pub fn stride_scheduling_task() -> Option<Arc<TaskControlBlock>> {
    TASK_MANAGER.exclusive_access().stride_scheduling()
}
