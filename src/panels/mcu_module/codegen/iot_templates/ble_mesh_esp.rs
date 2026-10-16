
// ── The ESP's shared flash, as the flash store's `verify` reads it ────────
// `flash_store::verify(&mut flash)` takes embedded-storage's byte-level
// traits, which esp-storage's flash has too: the same handle passes them on.

impl embedded_storage::ReadStorage for Flash {
    type Error = <StoreFlash as embedded_storage::ReadStorage>::Error;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.with(|f| embedded_storage::ReadStorage::read(f, offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.with(|f| embedded_storage::ReadStorage::capacity(f))
    }
}

impl embedded_storage::Storage for Flash {
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.with(|f| embedded_storage::Storage::write(f, offset, bytes))
    }
}
