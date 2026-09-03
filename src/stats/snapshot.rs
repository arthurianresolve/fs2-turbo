impl crate::FsStats {
    /// Returns the number of free bytes in the file system containing the provided path.
    #[inline]
    pub fn free_space(&self) -> u64 {
        self.free_space
    }
    /// Returns the available space in bytes to non-privileged users in the file system containing the provided path.
    #[inline]
    pub fn available_space(&self) -> u64 {
        self.available_space
    }
}
