//! Implementation of [`Processor`] and Intersection of control flow
//!
//! Here, the continuous operation of user apps in CPU is maintained,
//! the current running state of CPU is recorded,
//! and the replacement and transfer of control flow of different applications are executed.

use super::__switch;
use super::{fetch_task, stride_scheduling_task, TaskStatus};
use super::{TaskContext, TaskControlBlock};
use crate::config::{BIG_STRIDE, MAX_SYSCALL_NUM};
use crate::sync::UPSafeCell;
use crate::timer::get_time_us;
use crate::trap::TrapContext;
use alloc::sync::Arc;
use lazy_static::*;

/// Processor management structure
pub struct Processor {
    /// The task currently executing on the current processor
    current: Option<Arc<TaskControlBlock>>,
    /// The basic control flow of each core, helping to select and switch process
    idle_task_cx: TaskContext,
}

impl Processor {
    pub fn new() -> Self {
        Self {
            current: None,
            idle_task_cx: TaskContext::zero_init(),
        }
    }
    fn get_idle_task_cx_ptr(&mut self) -> *mut TaskContext {
        &mut self.idle_task_cx as *mut _
    }
    pub fn take_current(&mut self) -> Option<Arc<TaskControlBlock>> {
        self.current.take()
    }
    pub fn current(&self) -> Option<Arc<TaskControlBlock>> {
        self.current.as_ref().map(|task| Arc::clone(task))
    }

    /// Get the status of current task
    fn get_status_of_current_task(&self) -> TaskStatus {
        TaskStatus::Running
    }

    /// Get the syscall_times of current task
    fn get_syscall_times_of_current_task(&self) -> [u32; MAX_SYSCALL_NUM] {
        self.current
            .as_ref()
            .unwrap()
            .inner_exclusive_access()
            .syscall_times
    }

    /// Get the start_time of current task
    fn get_start_time_of_current_task(&self) -> usize {
        self.current
            .as_ref()
            .unwrap()
            .inner_exclusive_access()
            .start_time
    }

    fn plus_one_to_syscall_used(&mut self, syscall_id: usize) {
        self.current
            .as_mut()
            .unwrap()
            .inner_exclusive_access()
            .syscall_times[syscall_id] += 1;
    }

    fn initialize_start_time_of_current_task(&mut self) {
        if self
            .current
            .as_ref()
            .unwrap()
            .inner_exclusive_access()
            .start_time
            == 0
        {
            self.current
                .as_mut()
                .unwrap()
                .inner_exclusive_access()
                .start_time = get_time_us();
        }
    }

    fn set_priority_for_current_task(&mut self, prio: usize) {
        self.current
            .as_mut()
            .unwrap()
            .inner_exclusive_access()
            .schedule
            .prio = prio;
        self.current
            .as_mut()
            .unwrap()
            .inner_exclusive_access()
            .schedule
            .stride = BIG_STRIDE / prio;
    }

    fn mmap(&mut self, start: usize, len: usize, port: usize) -> isize {
        let memory_set = &mut self
            .current
            .as_mut()
            .unwrap()
            .inner_exclusive_access()
            .memory_set;
        memory_set.mmap(start, len, port)
    }

    fn munmap(&mut self, start: usize, len: usize) -> isize {
        let memory_set = &mut self
            .current
            .as_mut()
            .unwrap()
            .inner_exclusive_access()
            .memory_set;
        memory_set.munmap(start, len)
    }
}

lazy_static! {
    /// PROCESSOR instance through lazy_static!
    pub static ref PROCESSOR: UPSafeCell<Processor> = unsafe { UPSafeCell::new(Processor::new()) };
}

/// The main part of process execution and scheduling
///
/// Loop fetch_task to get the process that needs to run,
/// and switch the process through __switch
pub fn run_tasks() {
    loop {
        let mut processor = PROCESSOR.exclusive_access();
        if let Some(task) = stride_scheduling_task() {
            let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
            // access coming task TCB exclusively
            let mut task_inner = task.inner_exclusive_access();
            let next_task_cx_ptr = &task_inner.task_cx as *const TaskContext;
            task_inner.task_status = TaskStatus::Running;
            drop(task_inner);
            // release coming task TCB manually
            processor.current = Some(task);
            // release processor manually

            drop(processor);

            // initialize the start time of current task
            initialize_start_time_of_current_task();

            unsafe {
                __switch(idle_task_cx_ptr, next_task_cx_ptr);
            }
        }
    }
}

/// Get current task through take, leaving a None in its place
pub fn take_current_task() -> Option<Arc<TaskControlBlock>> {
    PROCESSOR.exclusive_access().take_current()
}

/// Get a copy of the current task
pub fn current_task() -> Option<Arc<TaskControlBlock>> {
    PROCESSOR.exclusive_access().current()
}

/// Get token of the address space of current task
pub fn current_user_token() -> usize {
    let task = current_task().unwrap();
    let token = task.inner_exclusive_access().get_user_token();
    token
}

/// Get the mutable reference to trap context of current task
pub fn current_trap_cx() -> &'static mut TrapContext {
    current_task()
        .unwrap()
        .inner_exclusive_access()
        .get_trap_cx()
}

/// Return to idle control flow for new scheduling
pub fn schedule(switched_task_cx_ptr: *mut TaskContext) {
    let mut processor = PROCESSOR.exclusive_access();
    let idle_task_cx_ptr = processor.get_idle_task_cx_ptr();
    drop(processor);
    unsafe {
        __switch(switched_task_cx_ptr, idle_task_cx_ptr);
    }
}

/// Get the mutable reference to trap context of current task
pub fn set_priority_for_current_task(prio: isize) -> isize {
    PROCESSOR
        .exclusive_access()
        .set_priority_for_current_task(prio as usize);
    prio
}

/// Get the status of current task
pub fn get_status_of_current_task() -> TaskStatus {
    PROCESSOR.exclusive_access().get_status_of_current_task()
}

/// Get the syscall_times of current task
pub fn get_syscall_times_of_current_task() -> [u32; MAX_SYSCALL_NUM] {
    PROCESSOR
        .exclusive_access()
        .get_syscall_times_of_current_task()
}

/// Get the start_time of current task
pub fn get_start_time_of_current_task() -> usize {
    PROCESSOR
        .exclusive_access()
        .get_start_time_of_current_task()
}

/// 当一个系统调用被调用时，给它的调用次数加一
pub fn plus_one_to_syscall_used(syscall_id: usize) {
    PROCESSOR
        .exclusive_access()
        .plus_one_to_syscall_used(syscall_id)
}

/// 记录task在CPU中第一次运行的时刻
pub fn initialize_start_time_of_current_task() {
    PROCESSOR
        .exclusive_access()
        .initialize_start_time_of_current_task();
}

pub fn mmap(start: usize, len: usize, port: usize) -> isize {
    PROCESSOR.exclusive_access().mmap(start, len, port)
}

pub fn munmap(start: usize, len: usize) -> isize {
    PROCESSOR.exclusive_access().munmap(start, len)
}


