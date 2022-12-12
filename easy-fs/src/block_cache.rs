use super::{BlockDevice, BLOCK_SZ};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use lazy_static::*;
use spin::Mutex;

/// Cached block inside memory
pub struct BlockCache {
    /// cached block data
    /// 位于内存中的缓冲区
    cache: [u8; BLOCK_SZ],
    /// underlying block id
    /// 记录了这个块缓存来自于磁盘中的块的编号
    block_id: usize,
    /// underlying block device
    /// /底层块设备的引用，可通过它进行块读写
    block_device: Arc<dyn BlockDevice>,
    /// whether the block is dirty
    /// 记录这个块从磁盘载入内存缓存之后，它有没有被修改过
    modified: bool,
}

impl BlockCache {
    /// Load a new BlockCache from disk.
    pub fn new(block_id: usize, block_device: Arc<dyn BlockDevice>) -> Self {
        let mut cache = [0u8; BLOCK_SZ];
        block_device.read_block(block_id, &mut cache);
        Self {
            cache,
            block_id,
            block_device,
            modified: false,
        }
    }
    /// Get the address of an offset inside the cached block data
    /// 得到一个 BlockCache 内部的缓冲区中指定偏移量 offset 的字节地址
    fn addr_of_offset(&self, offset: usize) -> usize {
        &self.cache[offset] as *const _ as usize
    }

    /// 获取缓冲区中的位于偏移量 offset 的一个类型为 T 的磁盘上数据结构的不可变引用
    /// 该泛型方法的 Trait Bound 限制类型 T 必须是一个编译时已知大小的类型
    pub fn get_ref<T>(&self, offset: usize) -> &T
    where
        T: Sized,
    {
        let type_size = core::mem::size_of::<T>(); // 在编译时获取类型 T 的大小
        assert!(offset + type_size <= BLOCK_SZ); // 确认该数据结构被整个包含在磁盘块及其缓冲区之内
        let addr = self.addr_of_offset(offset);
        unsafe { &*(addr as *const T) }
    }

    /// 获取磁盘上数据结构的可变引用
    pub fn get_mut<T>(&mut self, offset: usize) -> &mut T
    where
        T: Sized,
    {
        let type_size = core::mem::size_of::<T>();
        assert!(offset + type_size <= BLOCK_SZ);
        self.modified = true; // 由于这些数据结构目前位于内存中的缓冲区中，我们需要将 BlockCache 的 modified 标记为 true 表示该缓冲区已经被修改
        let addr = self.addr_of_offset(offset);
        unsafe { &mut *(addr as *mut T) }
    }

    pub fn read<T, V>(&self, offset: usize, f: impl FnOnce(&T) -> V) -> V {
        f(self.get_ref(offset))
    }

    /// 修改本block中从offset开始的大小跟T一样大的位置
    pub fn modify<T, V>(&mut self, offset: usize, f: impl FnOnce(&mut T) -> V) -> V {
        f(self.get_mut(offset))
    }
    /// 将本缓存区中的所有数据更新到磁盘中（若block中的数据被修改了的话）
    pub fn sync(&mut self) {
        if self.modified {
            self.modified = false;
            self.block_device.write_block(self.block_id, &self.cache);
        }
    }
}

impl Drop for BlockCache {
    fn drop(&mut self) {
        self.sync()
    }
}

/// Use a block cache of 16 blocks
const BLOCK_CACHE_SIZE: usize = 16;

pub struct BlockCacheManager {
    queue: VecDeque<(usize, Arc<Mutex<BlockCache>>)>,
}

impl BlockCacheManager {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }
    /// 寻找对应与block_id的BlockCache，如果block_id对应的Block还没有缓存到内存，就先将块设备上的block读到缓存
    pub fn get_block_cache(
        &mut self,
        block_id: usize,
        block_device: Arc<dyn BlockDevice>,
    ) -> Arc<Mutex<BlockCache>> {
        // 遍历整个队列试图找到一个编号相同的块缓存，如果找到了，会将块缓存管理器中保存的块缓存的引用复制一份并返回
        if let Some(pair) = self.queue.iter().find(|pair| pair.0 == block_id) {
            Arc::clone(&pair.1)
        } else {
            // substitute
            // 对应找不到的情况，此时必须将块从磁盘读入内存中的缓冲区。在实际读取之前，需要判断管理器保存的块缓存数量是否已经达到了上限
            if self.queue.len() == BLOCK_CACHE_SIZE {
                // from front to tail
                if let Some((idx, _)) = self
                    .queue
                    .iter()
                    .enumerate()
                    .find(|(_, pair)| Arc::strong_count(&pair.1) == 1)
                {
                    self.queue.drain(idx..=idx); // 此处当将一个block缓存移出queue后，Rsut会自动调用相关的Drop()函数处理的，如果数据被修改，Drop()函数就会将数据刷回磁盘中
                } else {
                    panic!("Run out of BlockCache!");
                }
            }
            // load block into mem and push back
            let block_cache = Arc::new(Mutex::new(BlockCache::new(
                block_id,
                Arc::clone(&block_device),
            )));
            self.queue.push_back((block_id, Arc::clone(&block_cache)));
            block_cache
        }
    }
}

lazy_static! {
    /// The global block cache manager
    pub static ref BLOCK_CACHE_MANAGER: Mutex<BlockCacheManager> = Mutex::new(
        BlockCacheManager::new()
    );
}

/// Get the block cache corresponding to the given block id and block device
/// 获得block_id对应的在缓存区中的blockcache, 如果缓存区中没有的话就先去磁盘中读到缓存区中
pub fn get_block_cache(
    block_id: usize,
    block_device: Arc<dyn BlockDevice>,
) -> Arc<Mutex<BlockCache>> {
    BLOCK_CACHE_MANAGER
        .lock()
        .get_block_cache(block_id, block_device)
}

/// Sync all block cache to block device
/// 将缓存区中的所有数据都更新到磁盘中
pub fn block_cache_sync_all() {
    let manager = BLOCK_CACHE_MANAGER.lock();
    for (_, cache) in manager.queue.iter() {
        cache.lock().sync();
    }
}
