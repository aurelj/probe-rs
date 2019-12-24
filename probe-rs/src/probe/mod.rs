pub mod daplink;
pub mod stlink;

use crate::coresight::{
    access_ports::{
        custom_ap::{CtrlAP, ERASEALL, ERASEALLSTATUS, RESET},
        generic_ap::{APClass, APType, GenericAP, IDR},
        memory_ap::MemoryAP,
        APRegister,
    },
    ap_access::{get_ap_by_idr, APAccess, AccessPort},
    common::Register,
    memory::{adi_v5_memory_interface::ADIMemoryInterface, MI},
};

use log::debug;

use crate::error::*;
use colored::*;
use std::time::Instant;

#[derive(Copy, Clone, Debug)]
pub enum WireProtocol {
    Swd,
    Jtag,
}

const UNLOCK_TIMEOUT: u64 = 15;
const CTRL_AP_IDR: IDR = IDR {
    REVISION: 0,
    DESIGNER: 0x0144,
    CLASS: APClass::Undefined,
    _RES0: 0,
    VARIANT: 0,
    TYPE: APType::JTAG_COM_AP,
};

#[derive(Debug, PartialEq)]
pub enum Port {
    DebugPort,
    AccessPort(u16),
}

pub trait DAPAccess {
    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, port: Port, addr: u16) -> Result<u32>;

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(&mut self, port: Port, addr: u16, value: u32) -> Result<()>;
}

pub struct MasterProbe {
    actual_probe: Box<dyn DebugProbe>,
    current_apsel: u8,
    current_apbanksel: u8,
}

impl MasterProbe {
    pub fn from_specific_probe(probe: Box<dyn DebugProbe>) -> Self {
        MasterProbe {
            actual_probe: probe,
            current_apbanksel: 0,
            current_apsel: 0,
        }
    }

    pub fn target_reset(&mut self) -> Result<()> {
        self.actual_probe.target_reset()
    }

    fn select_ap_and_ap_bank(&mut self, port: u8, ap_bank: u8) -> Result<()> {
        let mut cache_changed = if self.current_apsel != port {
            self.current_apsel = port;
            true
        } else {
            false
        };

        if self.current_apbanksel != ap_bank {
            self.current_apbanksel = ap_bank;
            cache_changed = true;
        }

        if cache_changed {
            use crate::coresight::debug_port::Select;

            let mut select = Select(0);

            debug!(
                "Changing AP to {}, AP_BANK_SEL to {}",
                self.current_apsel, self.current_apbanksel
            );

            select.set_ap_sel(self.current_apsel);
            select.set_ap_bank_sel(self.current_apbanksel);

            self.actual_probe.write_register(
                Port::DebugPort,
                u16::from(Select::ADDRESS),
                select.into(),
            )?;
        }

        Ok(())
    }

    fn write_register_ap<AP, REGISTER>(&mut self, port: AP, register: REGISTER) -> Result<()>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>,
    {
        let register_value = register.into();

        debug!(
            "Writing register {}, value=0x{:08X}",
            REGISTER::NAME,
            register_value
        );

        self.select_ap_and_ap_bank(port.get_port_number(), REGISTER::APBANKSEL)?;

        let link = &mut self.actual_probe;
        link.write_register(
            Port::AccessPort(u16::from(self.current_apsel)),
            u16::from(REGISTER::ADDRESS),
            register_value,
        )?;
        Ok(())
    }

    fn read_register_ap<AP, REGISTER>(&mut self, port: AP, _register: REGISTER) -> Result<REGISTER>
    where
        AP: AccessPort,
        REGISTER: APRegister<AP>,
    {
        debug!("Reading register {}", REGISTER::NAME);
        self.select_ap_and_ap_bank(port.get_port_number(), REGISTER::APBANKSEL)?;

        let link = &mut self.actual_probe;
        //println!("{:?}, {:08X}", link.current_apsel, REGISTER::ADDRESS);
        let result = link.read_register(
            Port::AccessPort(u16::from(self.current_apsel)),
            u16::from(REGISTER::ADDRESS),
        )?;

        debug!(
            "Read register    {}, value=0x{:08x}",
            REGISTER::NAME,
            result
        );

        Ok(REGISTER::from(result))
    }

    pub fn read_register_dp(&mut self, offset: u16) -> Result<u32> {
        self.actual_probe.read_register(Port::DebugPort, offset)
    }

    pub fn write_register_dp(&mut self, offset: u16, val: u32) -> Result<()> {
        self.actual_probe
            .write_register(Port::DebugPort, offset, val)
    }

    /// Tries to mass erase a locked nRF52 chip, this process may timeout, if it does, the chip
    /// might be unlocked or not, it is advised to try again if flashing fails
    pub fn nrf_recover(&mut self) -> Result<()> {
        let ctrl_port = match get_ap_by_idr(self, |idr| idr == CTRL_AP_IDR) {
            Some(port) => CtrlAP::from(port),
            None => return res!(NotFound(NotFoundKind::CtrlAp)),
        };
        println!("Starting mass erase...");
        let mut erase_reg = ERASEALL::from(1);
        let status_reg = ERASEALLSTATUS::from(0);
        let mut reset_reg = RESET::from(1);

        // Reset first
        self.write_register_ap(ctrl_port, reset_reg)?;
        reset_reg.RESET = false;
        self.write_register_ap(ctrl_port, reset_reg)?;

        self.write_register_ap(ctrl_port, erase_reg)?;

        // Prepare timeout
        let now = Instant::now();
        let status = self.read_register_ap(ctrl_port, status_reg)?;
        log::info!("Erase status: {:?}", status.ERASEALLSTATUS);
        let timeout = loop {
            let status = self.read_register_ap(ctrl_port, status_reg)?;
            if !status.ERASEALLSTATUS {
                break false;
            }
            if now.elapsed().as_secs() >= UNLOCK_TIMEOUT {
                break true;
            }
        };
        reset_reg.RESET = true;
        self.write_register_ap(ctrl_port, reset_reg)?;
        reset_reg.RESET = false;
        self.write_register_ap(ctrl_port, reset_reg)?;
        erase_reg.ERASEALL = false;
        self.write_register_ap(ctrl_port, erase_reg)?;
        if timeout {
            eprintln!(
                "    {} Mass erase process timeout, the chip might still be locked.",
                "Error".red().bold()
            );
        } else {
            println!("Mass erase completed, chip unlocked");
        }
        Ok(())
    }
}

impl<REGISTER> APAccess<MemoryAP, REGISTER> for MasterProbe
where
    REGISTER: APRegister<MemoryAP>,
{
    fn read_register_ap(&mut self, port: MemoryAP, register: REGISTER) -> Result<REGISTER> {
        self.read_register_ap(port, register)
    }

    fn write_register_ap(&mut self, port: MemoryAP, register: REGISTER) -> Result<()> {
        self.write_register_ap(port, register)
    }
}

impl<REGISTER> APAccess<GenericAP, REGISTER> for MasterProbe
where
    REGISTER: APRegister<GenericAP>,
{
    fn read_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<REGISTER> {
        self.read_register_ap(port, register)
    }

    fn write_register_ap(&mut self, port: GenericAP, register: REGISTER) -> Result<()> {
        self.write_register_ap(port, register)
    }
}

impl MI for MasterProbe {
    fn read32(&mut self, address: u32) -> Result<u32> {
        ADIMemoryInterface::new(0).read32(self, address)
    }

    fn read8(&mut self, address: u32) -> Result<u8> {
        ADIMemoryInterface::new(0).read8(self, address)
    }

    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<()> {
        ADIMemoryInterface::new(0).read_block32(self, address, data)
    }

    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<()> {
        ADIMemoryInterface::new(0).read_block8(self, address, data)
    }

    fn write32(&mut self, addr: u32, data: u32) -> Result<()> {
        ADIMemoryInterface::new(0).write32(self, addr, data)
    }

    fn write8(&mut self, addr: u32, data: u8) -> Result<()> {
        ADIMemoryInterface::new(0).write8(self, addr, data)
    }

    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<()> {
        ADIMemoryInterface::new(0).write_block32(self, addr, data)
    }

    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        ADIMemoryInterface::new(0).write_block8(self, addr, data)
    }
}

pub trait DebugProbe: DAPAccess {
    fn new_from_probe_info(info: &DebugProbeInfo) -> Result<Box<Self>>
    where
        Self: Sized;

    /// Get human readable name for the probe
    fn get_name(&self) -> &str;

    /// Enters debug mode
    fn attach(&mut self, protocol: Option<WireProtocol>) -> Result<WireProtocol>;

    /// Leave debug mode
    fn detach(&mut self) -> Result<()>;

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<()>;
}

#[derive(Debug, Clone)]
pub enum DebugProbeType {
    DAPLink,
    STLink,
}

#[derive(Clone)]
pub struct DebugProbeInfo {
    pub identifier: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub probe_type: DebugProbeType,
}

impl std::fmt::Debug for DebugProbeInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{} (VID: {}, PID: {}, {}{:?})",
            self.identifier,
            self.vendor_id,
            self.product_id,
            self.serial_number
                .clone()
                .map_or("".to_owned(), |v| format!("Serial: {},", v)),
            self.probe_type
        )
    }
}

impl DebugProbeInfo {
    pub fn new<S: Into<String>>(
        identifier: S,
        vendor_id: u16,
        product_id: u16,
        serial_number: Option<String>,
        probe_type: DebugProbeType,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            vendor_id,
            product_id,
            serial_number,
            probe_type,
        }
    }
}

#[derive(Default)]
pub struct FakeProbe;

impl FakeProbe {
    pub fn new() -> Self {
        Self::default()
    }
}

impl DebugProbe for FakeProbe {
    fn new_from_probe_info(_info: &DebugProbeInfo) -> Result<Box<Self>>
    where
        Self: Sized,
    {
        res!(ProbeCouldNotBeCreated)
    }

    /// Get human readable name for the probe
    fn get_name(&self) -> &str {
        "Mock probe for testing"
    }

    /// Enters debug mode
    fn attach(&mut self, protocol: Option<WireProtocol>) -> Result<WireProtocol> {
        // attaching always work for the fake probe
        Ok(protocol.unwrap_or(WireProtocol::Swd))
    }

    /// Leave debug mode
    fn detach(&mut self) -> Result<()> {
        Ok(())
    }

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<()> {
        res!(UnknownError)
    }
}

impl DAPAccess for FakeProbe {
    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, _port: Port, _addr: u16) -> Result<u32> {
        res!(UnknownError)
    }

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(&mut self, _port: Port, _addr: u16, _value: u32) -> Result<()> {
        res!(UnknownError)
    }
}
