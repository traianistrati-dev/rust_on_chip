/// The pico2-ice's FPGA loader: sends a bitstream into the iCE40UP5K's
/// configuration RAM through its slave-SPI port, the sequence of Lattice
/// FPGA-TN-02001 section 13.2 (and of tinyVision's own `ice_cram.c`).
///
/// Bit-banged on purpose. GP4, the line INTO the FPGA, is SPI0 RX only, so no
/// SPI block can drive it; this needs nothing but a few outputs and an input,
/// and works the same on either runtime. SPI mode 3: SCK idles high, data
/// changes while SCK is low, the FPGA samples on the rising edge, MSB first.
struct Ice40Cram<R, S, C, D, N> {
    /// `CRESET_B` (GP31). LOW holds the FPGA in reset; the board pulls it low too.
    creset: R,
    /// `SPI_SS` (GP5). ALSO the FPGA flash's chip select - the same net.
    ss: S,
    /// `SPI_SCK` (GP6), shared with the FPGA flash.
    sck: C,
    /// `SPI_SI` (GP4): data into the FPGA. The flash's data OUT on the same net.
    si: D,
    /// `CDONE` (GP40): high once the FPGA holds a configuration.
    cdone: N,
}

/// One byte out, MSB first: data set while SCK is low, sampled on the rise.
fn ice40_shift<C, D>(sck: &mut C, data: &mut D, byte: u8, half: u32, wait: &mut impl FnMut(u32))
where
    C: embedded_hal::digital::OutputPin,
    D: embedded_hal::digital::OutputPin,
{
    for bit in (0..8).rev() {
        let _ = sck.set_low();
        if (byte >> bit) & 1 == 1 {
            let _ = data.set_high();
        } else {
            let _ = data.set_low();
        }
        wait(half);
        let _ = sck.set_high();
        wait(half);
    }
}

impl<R, S, C, D, N> Ice40Cram<R, S, C, D, N>
where
    R: embedded_hal::digital::OutputPin,
    S: embedded_hal::digital::OutputPin,
    C: embedded_hal::digital::OutputPin,
    D: embedded_hal::digital::OutputPin,
    N: embedded_hal::digital::InputPin,
{
    /// Holds the FPGA in reset and sends its flash to sleep (0xB9, deep
    /// power-down) on `so`, the flash's data in (GP7).
    ///
    /// The flash shares SS and SCK with the FPGA, and with nothing to put it
    /// to sleep it is awake from power-up. Asleep, it ignores everything but a
    /// wake-up command, so `so` can be released (to a pulled-down input) before
    /// the load: from then on it is the FPGA's `SPI_SO`, which the FPGA drives
    /// itself once configured.
    ///
    /// `half` is half an SCK period in CPU cycles, paced by `wait`, which only
    /// sets how fast the clock runs. `settle_us` never returns early: it
    /// carries the FPGA's hard minimums, in microseconds.
    fn ice40_sleep_flash(
        &mut self,
        so: &mut impl embedded_hal::digital::OutputPin,
        half: u32,
        wait: &mut impl FnMut(u32),
        settle_us: &mut impl FnMut(u32),
    ) {
        let _ = self.creset.set_low();
        let _ = self.ss.set_high();
        let _ = self.sck.set_high();
        let _ = self.si.set_low();
        let _ = so.set_low();
        settle_us(2);
        let _ = self.ss.set_low();
        ice40_shift(&mut self.sck, so, 0xB9, half, wait);
        let _ = so.set_low();
        let _ = self.ss.set_high();
        settle_us(5);
    }

    /// Loads `image` and says whether the FPGA took it (CDONE high). Call
    /// [`Self::ice40_sleep_flash`] first.
    ///
    /// On failure CRESET goes back LOW, as tinyVision's own firmware does: the
    /// FPGA stays in reset rather than half-configured.
    fn ice40_load(
        &mut self,
        half: u32,
        wait: &mut impl FnMut(u32),
        settle_us: &mut impl FnMut(u32),
        image: &[u8],
    ) -> bool {
        // SS low while CRESET rises is what selects SLAVE mode; then the FPGA
        // clears its configuration memory for at least 1200 us.
        let _ = self.ss.set_low();
        settle_us(2);
        let _ = self.creset.set_high();
        settle_us(1300);

        // Eight dummy clocks with SS high, then the image with SS low.
        let _ = self.ss.set_high();
        ice40_shift(&mut self.sck, &mut self.si, 0, half, wait);
        let _ = self.ss.set_low();
        for &byte in image {
            ice40_shift(&mut self.sck, &mut self.si, byte, half, wait);
        }

        // SS high, then clocks until CDONE rises (up to 100), then at least 49
        // more before the FPGA's I/O is released to the design.
        let _ = self.ss.set_high();
        for _ in 0..13 {
            ice40_shift(&mut self.sck, &mut self.si, 0, half, wait);
            if self.cdone.is_high().unwrap_or(false) {
                for _ in 0..7 {
                    ice40_shift(&mut self.sck, &mut self.si, 0, half, wait);
                }
                return true;
            }
        }
        let _ = self.creset.set_low();
        false
    }

    /// The lines back, to be released or kept.
    fn ice40_release(self) -> (R, S, C, D, N) {
        (self.creset, self.ss, self.sck, self.si, self.cdone)
    }
}
