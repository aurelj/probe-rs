use std::result;

use super::builder::FlashBuilder;
use crate::config::{
    flash_algorithm::FlashAlgorithm,
    memory::{FlashRegion, MemoryRange},
    target::Target,
};
use crate::coresight::memory::MI;
use crate::error::*;
use crate::probe::MasterProbe;

pub trait Operation {
    fn operation() -> u32;
    fn operation_name(&self) -> &str {
        match Self::operation() {
            1 => "Erase",
            2 => "Program",
            3 => "Verify",
            _ => "Unknown Operation",
        }
    }
}

pub struct Erase;

impl Operation for Erase {
    fn operation() -> u32 {
        1
    }
}

pub struct Program;

impl Operation for Program {
    fn operation() -> u32 {
        2
    }
}

pub struct Verify;

impl Operation for Verify {
    fn operation() -> u32 {
        3
    }
}

pub struct Flasher<'a> {
    target: &'a Target,
    probe: &'a mut MasterProbe,
    flash_algorithm: &'a FlashAlgorithm,
    region: &'a FlashRegion,
    double_buffering_supported: bool,
}

impl<'a> Flasher<'a> {
    pub fn new(
        target: &'a Target,
        probe: &'a mut MasterProbe,
        flash_algorithm: &'a FlashAlgorithm,
        region: &'a FlashRegion,
    ) -> Self {
        Self {
            target,
            probe,
            flash_algorithm,
            region,
            double_buffering_supported: false,
        }
    }

    pub fn region(&self) -> &FlashRegion {
        &self.region
    }

    pub fn flash_algorithm(&self) -> &FlashAlgorithm {
        &self.flash_algorithm
    }

    pub fn double_buffering_supported(&self) -> bool {
        self.double_buffering_supported
    }

    pub fn init<'b, 's: 'b, O: Operation>(
        &'s mut self,
        mut address: Option<u32>,
        clock: Option<u32>,
    ) -> Result<ActiveFlasher<'b, O>> {
        log::debug!("Initializing the flash algorithm.");
        let flasher = self;
        let algo = flasher.flash_algorithm;

        use capstone::arch::*;
        let cs = capstone::Capstone::new()
            .arm()
            .mode(arm::ArchMode::Thumb)
            .endian(capstone::Endian::Little)
            .build()
            .unwrap();
        let i = algo
            .instructions
            .iter()
            .map(|i| {
                [
                    *i as u8,
                    (*i >> 8) as u8,
                    (*i >> 16) as u8,
                    (*i >> 24) as u8,
                ]
            })
            .collect::<Vec<[u8; 4]>>()
            .iter()
            .flatten()
            .copied()
            .collect::<Vec<u8>>();

        let instructions = cs
            .disasm_all(i.as_slice(), u64::from(algo.load_address))
            .unwrap();

        for instruction in instructions.iter() {
            log::debug!("{}", instruction);
        }

        if address.is_none() {
            address = Some(flasher.region.flash_info().rom_start);
        }

        // TODO: Halt & reset target.
        log::debug!("Halting core.");
        let cpu_info = flasher.target.core.halt(&mut flasher.probe);
        log::debug!("PC = 0x{:08x}", cpu_info.unwrap().pc);
        flasher
            .target
            .core
            .wait_for_core_halted(&mut flasher.probe)?;
        log::debug!("Reset and halt");
        flasher.target.core.reset_and_halt(&mut flasher.probe)?;

        // TODO: Possible special preparation of the target such as enabling faster clocks for the flash e.g.

        // Load flash algorithm code into target RAM.
        log::debug!(
            "Loading algorithm into RAM at address 0x{:08x}",
            algo.load_address
        );
        flasher
            .probe
            .write_block32(algo.load_address, algo.instructions.as_slice())?;

        let mut data = vec![0; algo.instructions.len()];
        flasher.probe.read_block32(algo.load_address, &mut data)?;

        assert_eq!(&algo.instructions, &data.as_slice());
        log::debug!("RAM contents match flashing algo blob.");

        log::debug!("Preparing Flasher for region:");
        log::debug!("{:#?}", &flasher.region);
        log::debug!(
            "Double buffering enabled: {}",
            flasher.double_buffering_supported
        );
        let mut flasher = ActiveFlasher {
            target: flasher.target,
            probe: flasher.probe,
            flash_algorithm: flasher.flash_algorithm,
            region: flasher.region,
            double_buffering_supported: flasher.double_buffering_supported,
            _operation: core::marker::PhantomData,
        };

        flasher.init(address, clock)?;

        Ok(flasher)
    }

    pub fn run_erase<T, E: From<Error>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Erase>) -> result::Result<T, E> + Sized,
    ) -> result::Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub fn run_program<T, E: From<Error>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Program>) -> result::Result<T, E> + Sized,
    ) -> result::Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub fn run_verify<T, E: From<Error>>(
        &mut self,
        f: impl FnOnce(&mut ActiveFlasher<Verify>) -> result::Result<T, E> + Sized,
    ) -> result::Result<T, E> {
        // TODO: Fix those values (None, None).
        let mut active = self.init(None, None)?;
        let r = f(&mut active)?;
        active.uninit()?;
        Ok(r)
    }

    pub fn flash_block(
        self,
        address: u32,
        data: &[u8],
        do_chip_erase: bool,
        _fast_verify: bool,
    ) -> Result<()> {
        if !self
            .region
            .range
            .contains_range(&(address..address + data.len() as u32))
        {
            return res!(AddressNotInRegion {
                address,
                region: self.region.clone(),
            });
        }

        let mut fb = FlashBuilder::new();
        fb.add_data(address, data).expect("Add Data failed");
        fb.program(self, do_chip_erase, true)
            .expect("Add Data failed");

        Ok(())
    }
}

pub struct ActiveFlasher<'a, O: Operation> {
    target: &'a Target,
    probe: &'a mut MasterProbe,
    flash_algorithm: &'a FlashAlgorithm,
    region: &'a FlashRegion,
    double_buffering_supported: bool,
    _operation: core::marker::PhantomData<O>,
}

impl<'a, O: Operation> ActiveFlasher<'a, O> {
    pub fn init(&mut self, address: Option<u32>, clock: Option<u32>) -> Result<()> {
        let algo = &self.flash_algorithm;

        // Execute init routine if one is present.
        if let Some(pc_init) = algo.pc_init {
            log::debug!("Running init routine.");
            let result = self.call_function_and_wait(
                pc_init,
                address,
                clock.or(Some(0)),
                Some(O::operation()),
                None,
                true,
            )?;

            if result != 0 {
                return res!(CallFailed {
                    call: "init".into(),
                    result
                });
            }
        }

        Ok(())
    }

    pub fn region(&self) -> &FlashRegion {
        &self.region
    }

    pub fn flash_algorithm(&self) -> &FlashAlgorithm {
        &&self.flash_algorithm
    }

    // pub fn session_mut(&mut self) -> &mut Session {
    //     &mut self.session
    // }

    pub fn uninit<'b, 's: 'b>(&'s mut self) -> Result<Flasher<'b>> {
        log::debug!("Running uninit routine.");
        let algo = &self.flash_algorithm;

        if let Some(pc_uninit) = algo.pc_uninit {
            let result = self.call_function_and_wait(
                pc_uninit,
                Some(O::operation()),
                None,
                None,
                None,
                false,
            )?;

            if result != 0 {
                return res!(CallFailed {
                    call: "uninit".into(),
                    result
                });
            }
        }

        Ok(Flasher {
            target: self.target,
            probe: self.probe,
            flash_algorithm: self.flash_algorithm,
            region: self.region,
            double_buffering_supported: self.double_buffering_supported,
        })
    }

    fn call_function_and_wait(
        &mut self,
        pc: u32,
        r0: Option<u32>,
        r1: Option<u32>,
        r2: Option<u32>,
        r3: Option<u32>,
        init: bool,
    ) -> Result<u32> {
        self.call_function(pc, r0, r1, r2, r3, init)?;
        self.wait_for_completion()
    }

    fn call_function(
        &mut self,
        pc: u32,
        r0: Option<u32>,
        r1: Option<u32>,
        r2: Option<u32>,
        r3: Option<u32>,
        init: bool,
    ) -> Result<()> {
        log::debug!(
            "Calling routine {:08x}({:?}, {:?}, {:?}, {:?}, init={})",
            pc,
            r0,
            r1,
            r2,
            r3,
            init
        );

        let algo = &self.flash_algorithm;
        let regs = self.target.core.registers();

        [
            (regs.PC, Some(pc)),
            (regs.R0, r0),
            (regs.R1, r1),
            (regs.R2, r2),
            (regs.R3, r3),
            (regs.R9, if init { Some(algo.static_base) } else { None }),
            (regs.SP, if init { Some(algo.begin_stack) } else { None }),
            (regs.LR, Some(algo.load_address + 1)),
        ]
        .iter()
        .map(|(addr, value)| {
            if let Some(v) = value {
                self.target
                    .core
                    .write_core_reg(&mut self.probe, *addr, *v)?;
                log::debug!(
                    "content of {:#x}: 0x{:08x} should be: 0x{:08x}",
                    addr.0,
                    self.target.core.read_core_reg(&mut self.probe, *addr)?,
                    *v
                );
                Ok(())
            } else {
                Ok(())
            }
        })
        .collect::<Result<Vec<()>>>()?;

        // Resume target operation.
        self.target.core.run(&mut self.probe)?;

        Ok(())
    }

    pub fn wait_for_completion(&mut self) -> Result<u32> {
        log::debug!("Waiting for routine call completion.");
        let regs = self.target.core.registers();

        while self
            .target
            .core
            .wait_for_core_halted(&mut self.probe)
            .is_err()
        {}

        let r = self.target.core.read_core_reg(&mut self.probe, regs.R0)?;
        Ok(r)
    }

    pub fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<()> {
        self.probe.read_block32(address, data)?;
        Ok(())
    }

    pub fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<()> {
        self.probe.read_block8(address, data)?;
        Ok(())
    }
}

impl<'a> ActiveFlasher<'a, Erase> {
    pub fn erase_all(&mut self) -> Result<()> {
        log::debug!("Erasing entire chip.");
        let flasher = self;
        let algo = flasher.flash_algorithm;

        if let Some(pc_erase_all) = algo.pc_erase_all {
            let result =
                flasher.call_function_and_wait(pc_erase_all, None, None, None, None, false)?;

            if result != 0 {
                res!(CallFailed {
                    call: "program_page".into(),
                    result
                })
            } else {
                Ok(())
            }
        } else {
            res!(EraseAllNotSupported)
        }
    }

    pub fn erase_sector(&mut self, address: u32) -> Result<()> {
        log::info!("Erasing sector at address 0x{:08x}.", address);
        let t1 = std::time::Instant::now();
        let flasher = self;
        let algo = flasher.flash_algorithm;

        let result = flasher.call_function_and_wait(
            algo.pc_erase_sector,
            Some(address),
            None,
            None,
            None,
            false,
        )?;
        log::info!(
            "Done erasing sector. Result is {}. This took {:?}",
            result,
            t1.elapsed()
        );

        if result != 0 {
            log::error!("Erasing of sector at address 0x{:x} failed.", address);
            res!(CallFailed {
                call: "program_page".into(),
                result
            })
        } else {
            Ok(())
        }
    }
}

impl<'a> ActiveFlasher<'a, Program> {
    pub fn program_page(&mut self, address: u32, bytes: &[u8]) -> Result<()> {
        let t1 = std::time::Instant::now();
        let flasher = self;
        let algo = flasher.flash_algorithm;

        log::info!("Flashing one page of size: {}", bytes.len());

        // Transfer the bytes to RAM.
        flasher.probe.write_block8(algo.begin_data, bytes)?;
        let result = flasher.call_function_and_wait(
            algo.pc_program_page,
            Some(address),
            Some(bytes.len() as u32),
            Some(algo.begin_data),
            None,
            false,
        )?;
        log::info!("Flashing took: {:?}", t1.elapsed());

        if result != 0 {
            log::error!("Programming page at address 0x{:x} failed.", address);
            res!(CallFailed {
                call: "program_page".into(),
                result
            })
        } else {
            Ok(())
        }
    }

    pub fn start_program_page_with_buffer(
        &mut self,
        address: u32,
        buffer_number: u32,
    ) -> Result<()> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() as u32 {
            log::error!(
                "Tried to load data into buffer {} when there is only {} buffers.",
                buffer_number,
                algo.page_buffers.len()
            );
            return res!(Flasher);
        }

        flasher.call_function(
            algo.pc_program_page,
            Some(address),
            Some(flasher.region.page_size),
            Some(algo.page_buffers[buffer_number as usize]),
            None,
            false,
        )?;

        Ok(())
    }

    pub fn load_page_buffer(
        &mut self,
        _address: u32,
        bytes: &[u8],
        buffer_number: u32,
    ) -> Result<()> {
        let flasher = self;
        let algo = flasher.flash_algorithm;

        // Check the buffer number.
        if buffer_number < algo.page_buffers.len() as u32 {
            return res!(Flasher);
        }

        // TODO: Prevent security settings from locking the device.

        // Transfer the buffer bytes to RAM.
        flasher
            .probe
            .write_block8(algo.page_buffers[buffer_number as usize], bytes)?;

        Ok(())
    }
}
