use alloc::sync::Arc;
use spin::Mutex;
use super::{
    BlockDevice,
    Bitmap,
    SuperBlock,
    DiskInode,
    DiskInodeType,
    Inode,
    get_block_cache,
    block_cache_sync_all,
};
use crate::BLOCK_SZ;

/// An easy fs over a block device
pub struct EasyFileSystem {
    pub block_device: Arc<dyn BlockDevice>,
    pub inode_bitmap: Bitmap,
    pub data_bitmap: Bitmap,
    inode_area_start_block: u32,
    data_area_start_block: u32,
}

/// A data block of block size
type DataBlock = [u8; BLOCK_SZ];

impl EasyFileSystem {
    /// Create a filesystem from a block device
    pub fn create(
        block_device: Arc<dyn BlockDevice>,
        total_blocks: u32,
        inode_bitmap_blocks: u32,
    ) -> Arc<Mutex<Self>> {
        // calculate block size of areas & create bitmaps
        let inode_bitmap = Bitmap::new(1, inode_bitmap_blocks as usize);
        let inode_num = inode_bitmap.maximum(); // 本索引位图区可表示多少个索引节点的状态
        let inode_area_blocks =
            ((inode_num * core::mem::size_of::<DiskInode>() + BLOCK_SZ - 1) / BLOCK_SZ) as u32; // 索引节点区中block总个数（向上取整）
        let inode_total_blocks = inode_bitmap_blocks + inode_area_blocks; // 索引区总的block个数
        let data_total_blocks = total_blocks - 1 - inode_total_blocks; // 磁盘中block总数减去超级块区域（占一个block）和索引区后剩下的都是数据区
        let data_bitmap_blocks = (data_total_blocks + 4096) / 4097; // 数据位图占的block个数
        let data_area_blocks = data_total_blocks - data_bitmap_blocks; // 实际用于存储数据的区域中block个数
        let data_bitmap = Bitmap::new(
            (1 + inode_bitmap_blocks + inode_area_blocks) as usize,
            data_bitmap_blocks as usize,
        );
        let mut efs = Self {
            block_device: Arc::clone(&block_device),
            inode_bitmap,
            data_bitmap,
            inode_area_start_block: 1 + inode_bitmap_blocks,
            data_area_start_block: 1 + inode_total_blocks + data_bitmap_blocks,
        };
        // clear all blocks
        // 将物理磁盘上的所有空间都初始化为0（其实是在缓存区中做这件事，但是缓存区的大小大概率会比磁盘大，
        // 所以当缓存区满了之后就会为了腾出新的缓存而将一些缓存刷新到磁盘中，但磁盘中总会有一些地方并没有立刻刷新为0）
        for i in 0..total_blocks {
            get_block_cache(
                i as usize,
                Arc::clone(&block_device)
            )
            .lock()
            .modify(0, |data_block: &mut DataBlock| {
                for byte in data_block.iter_mut() { *byte = 0; }
            });
        }
        // initialize SuperBlock
        // 初始化超级块：只占磁盘上的第一个block
        get_block_cache(0, Arc::clone(&block_device))
        .lock()
        .modify(0, |super_block: &mut SuperBlock| {
            super_block.initialize(
                total_blocks,
                inode_bitmap_blocks,
                inode_area_blocks,
                data_bitmap_blocks,
                data_area_blocks,
            );
        });
        // write back immediately
        // create a inode for root node "/"
        assert_eq!(efs.alloc_inode(), 0);  // 将索引位图的第一个bit置为1
        let (root_inode_block_id, root_inode_offset) = efs.get_disk_inode_pos(0);
        get_block_cache(
            root_inode_block_id as usize,
            Arc::clone(&block_device)
        )
        .lock()
        .modify(root_inode_offset, |disk_inode: &mut DiskInode| {
            disk_inode.initialize(DiskInodeType::Directory);
        });
        block_cache_sync_all();
        Arc::new(Mutex::new(efs))
    }
    /// Open a block device as a filesystem
    /// 从一个已写入了 easy-fs 镜像的块设备上打开我们的 easy-fs
    pub fn open(block_device: Arc<dyn BlockDevice>) -> Arc<Mutex<Self>> {
        // read SuperBlock
        get_block_cache(0, Arc::clone(&block_device))
            .lock()
            .read(0, |super_block: &SuperBlock| {
                assert!(super_block.is_valid(), "Error loading EFS!");
                let inode_total_blocks =
                    super_block.inode_bitmap_blocks + super_block.inode_area_blocks;
                let efs = Self {
                    block_device,
                    inode_bitmap: Bitmap::new(
                        1,
                        super_block.inode_bitmap_blocks as usize
                    ),
                    data_bitmap: Bitmap::new(
                        (1 + inode_total_blocks) as usize,
                        super_block.data_bitmap_blocks as usize,
                    ),
                    inode_area_start_block: 1 + super_block.inode_bitmap_blocks,
                    data_area_start_block: 1 + inode_total_blocks + super_block.data_bitmap_blocks,
                };
                Arc::new(Mutex::new(efs))
            })
    }
    /// Get the root inode of the filesystem
    /// 创建root对应的inode
    pub fn root_inode(efs: &Arc<Mutex<Self>>) -> Inode {
        let block_device = Arc::clone(&efs.lock().block_device);
        // acquire efs lock temporarily
        let (block_id, block_offset) = efs.lock().get_disk_inode_pos(0);
        // release efs lock
        Inode::new(
            block_id,
            block_offset,
            Arc::clone(efs),
            block_device,
        )
    }
    /// Get inode by id
    /// 获得此inode_id对应的DiskInode在磁盘中的block id和在block内的偏移量（每个block可以存储多个inode）
    pub fn get_disk_inode_pos(&self, inode_id: u32) -> (u32, usize) {
        let inode_size = core::mem::size_of::<DiskInode>();
        let inodes_per_block = (BLOCK_SZ / inode_size) as u32;
        let block_id = self.inode_area_start_block + inode_id / inodes_per_block;
        (block_id, (inode_id % inodes_per_block) as usize * inode_size)
    }
    /// Get data block by id
    /// 获得此data_block_id在整个块设备中的block_id
    pub fn get_data_block_id(&self, data_block_id: u32) -> u32 {
        self.data_area_start_block + data_block_id
    }
    pub fn get_inode_area_start_block(&self) -> u32{
        self.inode_area_start_block
    }
    /// Allocate a new inode
    /// 在索引位图上分配一个bit，并返回它对应的在索引区的inode的inode_id(也就是索引区的第几个索引，注意一个block中包含了多个inode)
    pub fn alloc_inode(&mut self) -> u32 {
        self.inode_bitmap.alloc(&self.block_device).unwrap() as u32
    }
    /// Allocate a data block
    /// 将data bitmap中的一个bit置0，并返回它对应的block_id
    pub fn alloc_data(&mut self) -> u32 {
        self.data_bitmap.alloc(&self.block_device).unwrap() as u32 + self.data_area_start_block
    }
    /// Deallocate a data block
    /// 将block_id对应的数据块中的所有字节置0，并将其对应的在bitmap中的位置置0
    pub fn dealloc_data(&mut self, block_id: u32) {
        get_block_cache(
            block_id as usize,
            Arc::clone(&self.block_device)
        )
        .lock()
        .modify(0, |data_block: &mut DataBlock| {
            data_block.iter_mut().for_each(|p| { *p = 0; })
        });
        self.data_bitmap.dealloc(
            &self.block_device,
            (block_id - self.data_area_start_block) as usize
        )
    }
}
