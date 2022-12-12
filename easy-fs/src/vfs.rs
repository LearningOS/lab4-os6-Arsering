use super::{
    block_cache_sync_all, get_block_cache, BlockDevice, DirEntry, DiskInode, DiskInodeType,
    EasyFileSystem, DIRENT_SZ,
};
use crate::BLOCK_SZ;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::{Mutex, MutexGuard};

/// Virtual filesystem layer over easy-fs
/// 每一个DiskInode都对应一个Inode，Inode记录了DiskInode在磁盘上的位置（在哪个磁盘上的哪个Block中的哪个位置）
pub struct Inode {
    block_id: usize,
    block_offset: usize,
    fs: Arc<Mutex<EasyFileSystem>>,
    block_device: Arc<dyn BlockDevice>,
}

impl Inode {
    /// Create a vfs inode
    pub fn new(
        block_id: u32,
        block_offset: usize,
        fs: Arc<Mutex<EasyFileSystem>>,
        block_device: Arc<dyn BlockDevice>,
    ) -> Self {
        Self {
            block_id: block_id as usize,
            block_offset,
            fs,
            block_device,
        }
    }
    /// Call a function over a disk inode to read it
    /// 找到Diskinode（就是调用者对应的Diskinode)所在的block cache获得DiskInode中的信息，然后根据这些信息去操纵跟它绑定的存在数据区的数据
    fn read_disk_inode<V>(&self, f: impl FnOnce(&DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .read(self.block_offset, f)
    }
    /// Call a function over a disk inode to modify it
    /// 找到本inode对应的Diskinode所在的block cache并从这个block cache的offset位置获得一个DiskInode的数据
    fn modify_disk_inode<V>(&self, f: impl FnOnce(&mut DiskInode) -> V) -> V {
        get_block_cache(self.block_id, Arc::clone(&self.block_device))
            .lock()
            .modify(self.block_offset, f)
    }
    /// Find inode under a disk inode by name
    /// 如果这个Inode对应的DiskInode对应的是一个目录，就根据给定的文件名在这个目录下寻找它对应的dirent，并返回存在dirent中的这个文件对应的DiskInode的inode_id
    fn find_inode_id(&self, name: &str, disk_inode: &DiskInode) -> Option<u32> {
        // assert it is a directory
        assert!(disk_inode.is_dir());
        let file_count = (disk_inode.size as usize) / DIRENT_SZ;
        let mut dirent = DirEntry::empty();
        for i in 0..file_count {
            assert_eq!(
                disk_inode.read_at(DIRENT_SZ * i, dirent.as_bytes_mut(), &self.block_device,),
                DIRENT_SZ,
            );
            if dirent.inode_number() != 0 && dirent.name() == name {
                return Some(dirent.inode_number() as u32);
            }
        }
        None
    }
    /// Find inode under current inode by name
    /// 找到这个名字代表的文件在块设备中的DiskInode，并返回相应的Inode(self对应的DiskInode必须是一个目录，否则会报错)
    pub fn find(&self, name: &str) -> Option<Arc<Inode>> {
        let fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            self.find_inode_id(name, disk_inode).map(|inode_id| {
                let (block_id, block_offset) = fs.get_disk_inode_pos(inode_id);
                Arc::new(Self::new(
                    block_id,
                    block_offset,
                    self.fs.clone(),
                    self.block_device.clone(),
                ))
            })
        })
    }
    /// Increase the size of a disk inode
    /// 向efs申请需要的在数据区的block，将这些block对应的id存到DiskInode中，并将这个block在data bitmap中的相应bit置1
    fn increase_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size < disk_inode.size {
            return;
        }
        let blocks_needed = disk_inode.blocks_num_needed(new_size);
        let mut v: Vec<u32> = Vec::new();
        for _ in 0..blocks_needed {
            v.push(fs.alloc_data());
        }
        disk_inode.increase_size(new_size, v, &self.block_device);
    }
    fn decrease_size(
        &self,
        new_size: u32,
        disk_inode: &mut DiskInode,
        fs: &mut MutexGuard<EasyFileSystem>,
    ) {
        if new_size > disk_inode.size {
            return;
        }
        let data_blocks_dealloc = disk_inode.decrease_size(new_size, &self.block_device);
        for data_block in data_blocks_dealloc.into_iter() {
            fs.dealloc_data(data_block);
        }
    }

    /// Create inode under current inode by name
    /// 其实是在构建一个对应的DiskInode，在最后返回一个Inode,
    pub fn create(&self, name: &str) -> Option<Arc<Inode>> {
        let mut fs = self.fs.lock();
        if self
            .modify_disk_inode(|root_inode| {
                // assert it is a directory
                assert!(root_inode.is_dir());
                // has the file been created?
                self.find_inode_id(name, root_inode)
            })
            .is_some()
        {
            return None;
        }
        // create a new file
        // alloc a inode with an indirect block
        let new_inode_id = fs.alloc_inode();
        // initialize inode
        // 创建一个对应的DiskInode并将其写入磁盘中（实际是写入对应的缓存区了）
        //首先根据DiskInode的id计算出它所在的block的id以及在block内的偏移
        let (new_inode_block_id, new_inode_block_offset) = fs.get_disk_inode_pos(new_inode_id);

        // 将这个DiskInode初始化（在内存缓存区中）
        get_block_cache(new_inode_block_id as usize, Arc::clone(&self.block_device))
            .lock()
            .modify(new_inode_block_offset, |new_inode: &mut DiskInode| {
                new_inode.initialize(DiskInodeType::File);
            });

        // 将文件（DiskInode）对应的DirEntry写入目录（self）指向的在数据区的block中
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(name, new_inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });

        let (block_id, block_offset) = fs.get_disk_inode_pos(new_inode_id);
        block_cache_sync_all();
        // return inode
        Some(Arc::new(Self::new(
            block_id,
            block_offset,
            self.fs.clone(),
            self.block_device.clone(),
        )))
        // release efs lock automatically by compiler
    }

    pub fn linkat(&self, oldpath: &str, newpath: &str) -> isize {
        let mut fs = self.fs.lock();
        let inode_id: u32;
        match self.read_disk_inode(|root_inode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            // has the file been created?
            self.find_inode_id(oldpath, root_inode)
        }) {
            Some(inode) => {
                inode_id = inode;
            }
            None => return -1,
        }
        self.modify_disk_inode(|root_inode| {
            // append file in the dirent
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            let new_size = (file_count + 1) * DIRENT_SZ;
            // increase size
            self.increase_size(new_size as u32, root_inode, &mut fs);
            // write dirent
            let dirent = DirEntry::new(newpath, inode_id);
            root_inode.write_at(
                file_count * DIRENT_SZ,
                dirent.as_bytes(),
                &self.block_device,
            );
        });
        0
    }
    /// 只能由目录的Inode调用
    pub fn unlinkat(&self, name: &str) -> isize {
        let mut fs = self.fs.lock();
        let mut mark = -1;
        self.modify_disk_inode(|root_inode| {
            // assert it is a directory
            assert!(root_inode.is_dir());

            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    root_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if dirent.name().eq(name) {
                    root_inode.read_at(
                        (file_count - 1) * DIRENT_SZ,
                        dirent.as_bytes_mut(),
                        &self.block_device,
                    );
                    root_inode.write_at(i * DIRENT_SZ, dirent.as_bytes(), &self.block_device);
                    self.decrease_size(((file_count - 1) * DIRENT_SZ) as u32, root_inode, &mut fs);
                    mark = 0;
                    break;
                }
            }
        });
        mark
    }

    pub fn get_diskinodetype(&self) -> (usize, bool) {
        let fs = self.fs.lock();

        let inode_size = core::mem::size_of::<DiskInode>();
        let inodes_per_block = (BLOCK_SZ / inode_size) as u32;
        let ino = (self.block_id - (fs.get_inode_area_start_block() as usize))
            * inodes_per_block as usize
            + self.block_offset / inode_size;
        let mut mode = false;
        self.read_disk_inode(|inode| {
            mode = inode.is_dir();
        });
        (ino, mode)
    }

    pub fn get_nlink(&self, inode_num: usize) -> usize {
        let _fs = self.fs.lock();
        let mut nlink = 0usize;
        self.read_disk_inode(|root_inode| {
            // assert it is a directory
            assert!(root_inode.is_dir());
            let file_count = (root_inode.size as usize) / DIRENT_SZ;
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    root_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                if inode_num == (dirent.inode_number() as usize) {
                    nlink += 1;
                }
            }
        });
        nlink
    }
    /// List inodes under current inode
    /// 只有目录项可以调用
    pub fn ls(&self) -> Vec<String> {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| {
            let file_count = (disk_inode.size as usize) / DIRENT_SZ;
            let mut v: Vec<String> = Vec::new();
            for i in 0..file_count {
                let mut dirent = DirEntry::empty();
                assert_eq!(
                    disk_inode.read_at(i * DIRENT_SZ, dirent.as_bytes_mut(), &self.block_device,),
                    DIRENT_SZ,
                );
                v.push(String::from(dirent.name()));
            }
            v
        })
    }
    /// Read data from current inode
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> usize {
        let _fs = self.fs.lock();
        self.read_disk_inode(|disk_inode| disk_inode.read_at(offset, buf, &self.block_device))
    }
    /// Write data to current inode
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> usize {
        let mut fs = self.fs.lock();
        let size = self.modify_disk_inode(|disk_inode| {
            self.increase_size((offset + buf.len()) as u32, disk_inode, &mut fs);
            disk_inode.write_at(offset, buf, &self.block_device)
        });
        block_cache_sync_all();
        size
    }
    /// Clear the data in current inode
    pub fn clear(&self) {
        let mut fs = self.fs.lock();
        self.modify_disk_inode(|disk_inode| {
            let size = disk_inode.size;
            let data_blocks_dealloc = disk_inode.clear_size(&self.block_device);
            assert!(data_blocks_dealloc.len() == DiskInode::total_blocks(size) as usize);
            for data_block in data_blocks_dealloc.into_iter() {
                fs.dealloc_data(data_block);
            }
        });
        block_cache_sync_all();
    }
}
