impl crate::FsStats {
    /// Returns the number of free bytes in the file system containing the provided path.
    #[inline]
    pub fn free_space(&self) -> u64 {
        self.free_space
    }
}
