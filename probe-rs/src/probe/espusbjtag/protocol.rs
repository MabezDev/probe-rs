use core::fmt;
use std::{fmt::Debug, time::Duration};

use rusb::{request_type, Context, Device, Direction, TransferType, UsbContext};

use crate::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, DebugProbeType, ProbeCreationError,
};

const JTAG_PROTOCOL_CAPABILITIES_VERSION: u8 = 1;
const JTAG_PROTOCOL_CAPABILITIES_SPEED_APB_TYPE: u8 = 1;
const MAX_COMMAND_REPETITIONS: usize = 1024;
const OUT_BUFFER_SIZE: usize = OUT_EP_BUFFER_SIZE * 1;
const OUT_EP_BUFFER_SIZE: usize = 64;
const IN_EP_BUFFER_SIZE: usize = 64;
const USB_TIMEOUT: Duration = Duration::from_millis(5000);
const USB_DEVICE_CLASS: u8 = 0xFF;
const USB_DEVICE_SUBCLASS: u8 = 0xFF;
const USB_DEVICE_PROTOCOL: u8 = 0x01;
const USB_DEVICE_TRANSFER_TYPE: TransferType = TransferType::Bulk;

const USB_CONFIGURATION: u8 = 0x0;

const USB_VID: u16 = 0x303A;
const USB_PID: u16 = 0x1001;

const VENDOR_DESCRIPTOR_JTAG_CAPABILITIES: u16 = 0x2000;

pub(super) struct ProtocolHandler {
    // The USB device handle.
    device_handle: rusb::DeviceHandle<rusb::Context>,

    // The command in the queue and their repetitions.
    // For now we do one command at a time.
    command_queue: Option<(Command, usize)>,
    // The buffer for all commands to be sent to the target. This already contains `repeated` commands which are basically
    // a mechanism to compress the datastream by adding a `Repeat` command to repeat the previous command `n` times instead of
    // actually putting the command into the queue `n` times.
    output_buffer: Vec<Command>,
    // A store for all the read bits (from the traget) such that the BitIter the methods return can borrow and iterate over it.
    pub(crate) input_buffers: Vec<OwnedBitIter>,
    pub(crate) pending_in_bits: usize,

    ep_out: u8,
    ep_in: u8,
}

impl Debug for ProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtocolHandler")
            .field("command_queue", &self.command_queue)
            .field("output_buffer", &self.output_buffer)
            .field("input_buffers", &self.input_buffers)
            .field("ep_out", &self.ep_out)
            .field("ep_in", &self.ep_in)
            .finish()
    }
}

impl ProtocolHandler {
    pub fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Self, ProbeCreationError> {
        let selector = selector.into();

        let context = Context::new()?;

        tracing::debug!("Acquired libusb context.");

        let device = context
            .devices()?
            .iter()
            .filter(is_espjtag_device)
            .find_map(|device| {
                let descriptor = device.device_descriptor().ok()?;
                // First match the VID & PID.
                if selector.vendor_id == descriptor.vendor_id()
                    && selector.product_id == descriptor.product_id()
                {
                    // If the VID & PID match, match the serial if one was given.
                    if let Some(serial) = &selector.serial_number {
                        let sn_str = read_serial_number(&device, &descriptor).ok();
                        if sn_str.as_ref() == Some(serial) {
                            Some(device)
                        } else {
                            None
                        }
                    } else {
                        // If no serial was given, the VID & PID match is enough; return the device.
                        Some(device)
                    }
                } else {
                    None
                }
            })
            .map_or(Err(ProbeCreationError::NotFound), Ok)?;

        let mut device_handle = device.open()?;

        tracing::debug!("Aquired handle for probe");

        let config = device.config_descriptor(USB_CONFIGURATION)?;

        tracing::debug!("Active config descriptor: {:?}", &config);

        let descriptor = device.device_descriptor()?;

        tracing::debug!("Device descriptor: {:?}", &descriptor);

        let mut ep_out = None;
        let mut ep_in = None;

        for interface in config.interfaces() {
            tracing::trace!("Interface {}", interface.number());
            let descriptor = interface.descriptors().next();
            if let Some(descriptor) = descriptor {
                if descriptor.class_code() == USB_DEVICE_CLASS
                    && descriptor.sub_class_code() == USB_DEVICE_SUBCLASS
                    && descriptor.protocol_code() == USB_DEVICE_PROTOCOL
                {
                    for endpoint in descriptor.endpoint_descriptors() {
                        tracing::trace!("Endpoint {}: {}", endpoint.number(), endpoint.address());
                        if endpoint.transfer_type() == USB_DEVICE_TRANSFER_TYPE {
                            if endpoint.direction() == Direction::In {
                                ep_in = Some(endpoint.address());
                            } else {
                                ep_out = Some(endpoint.address());
                            }
                        }
                    }
                }
            }

            if let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) {
                tracing::debug!(
                    "Claiming interface {} with IN EP {} and OUT EP {}.",
                    interface.number(),
                    ep_in,
                    ep_out
                );
                device_handle.claim_interface(interface.number())?;
            }
        }

        if let (Some(_), Some(_)) = (ep_in, ep_out) {
        } else {
            return Err(ProbeCreationError::ProbeSpecific(
                "USB interface or endpoints could not be found.".into(),
            ));
        }

        let mut buffer = [0; 255];
        device_handle.read_control(
            request_type(
                rusb::Direction::In,
                rusb::RequestType::Standard,
                rusb::Recipient::Device,
            ),
            rusb::constants::LIBUSB_REQUEST_GET_DESCRIPTOR,
            VENDOR_DESCRIPTOR_JTAG_CAPABILITIES,
            0,
            &mut buffer,
            USB_TIMEOUT,
        )?;

        // TODO:
        // let mut base_speed_khz = 1000;
        // let mut div_min = 1;
        // let mut div_max = 1;

        let protocol_version = buffer[0];
        tracing::debug!("{:?}", &buffer[..20]);
        tracing::debug!("Protocol version: {}", protocol_version);
        if protocol_version != JTAG_PROTOCOL_CAPABILITIES_VERSION {
            return Err(ProbeCreationError::ProbeSpecific(
                "Unknown capabilities descriptor version.".into(),
            ));
        }

        let length = buffer[1] as usize;

        let mut p = 2usize;
        while p < length {
            let typ = buffer[p];
            let length = buffer[p + 1];

            if typ == JTAG_PROTOCOL_CAPABILITIES_SPEED_APB_TYPE {
                // TODO:
                // base_speed_khz = (((buffer[p + 3] as u16) << 8) | buffer[p + 2] as u16) * 10 / 2;
                // div_min = ((buffer[p + 5] as u16) << 8) | buffer[p + 4] as u16;
                // div_max = ((buffer[p + 7] as u16) << 8) | buffer[p + 6] as u16;
            } else {
                tracing::warn!("Unknown capabilities type {:01X?}", typ);
            }

            p += length as usize;
        }

        // TODO:
        // let hw_in_fifo_len = 4;

        tracing::debug!("Succesfully attached to ESP USB JTAG.");

        Ok(Self {
            device_handle,
            command_queue: None,
            output_buffer: Vec::new(),
            input_buffers: Vec::new(),
            // The following expects are okay as we check that the values we call them on are `Some`.
            ep_out: ep_out.expect("This is a bug. Please report it."),
            ep_in: ep_in.expect("This is a bug. Please report it."),
            pending_in_bits: 0,
        })
    }

    /// Put a bit on TDI and possibly read one from TDO.
    pub fn jtag_io(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        cap: bool,
    ) -> Result<OwnedBitIter, DebugProbeError> {
        self.jtag_io_async(tms, tdi, cap)?;
        self.flush()
    }

    /// Put a bit on TDI and possibly read one from TDO.
    /// to recieve the bytes from this operations call [`ProtocolHandler::flush`]
    ///
    /// Note that if the internal buffer is exceeded bytes will be automatically flushed to usb device
    pub fn jtag_io_async(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        cap: bool,
    ) -> Result<(), DebugProbeError> {
        tracing::debug!("JTAG IO! {} ", cap);
        for (tms, tdi) in tms.into_iter().zip(tdi.into_iter()) {
            self.push_command(Command::Clock { cap, tdi, tms })?;
            if cap {
                self.pending_in_bits += 1;
            }
        }
        Ok(())
    }

    /// Sets the two different resets on the target.
    /// NOTE: Only srst can be set for now. Setting trst is not implemented yet.
    pub fn set_reset(&mut self, _trst: bool, srst: bool) -> Result<(), DebugProbeError> {
        // TODO: Handle trst using setup commands. This is not necessarily required and can be left as is for the moiment..
        self.push_command(Command::Reset(srst))?;
        self.flush()?;
        Ok(())
    }

    /// Adds a command to the command queue.
    /// This will properly add repeat commands if possible.
    fn push_command(&mut self, command: Command) -> Result<(), DebugProbeError> {
        if let Some((command_in_queue, repetitions)) = self.command_queue.as_mut() {
            if command == *command_in_queue && *repetitions < MAX_COMMAND_REPETITIONS {
                *repetitions += 1;
                return Ok(());
            } else {
                let command = *command_in_queue;
                let repetitions = *repetitions;
                self.write_stream(command, repetitions)?;
            }
        }

        self.command_queue = Some((command, 1));

        Ok(())
    }

    /// Flushes all the pending commands to the JTAG adapter.
    pub fn flush(&mut self) -> Result<OwnedBitIter, DebugProbeError> {
        if let Some((command_in_queue, repetitions)) = self.command_queue.take() {
            self.write_stream(command_in_queue, repetitions)?;
        }

        tracing::debug!("Flushing ...");

        // https://github.com/espressif/openocd-esp32/blob/a28f71785066722f49494e0d946fdc56966dcc0d/src/jtag/drivers/esp_usb_jtag.c#L423
        self.add_raw_command(Command::Flush)?;

        // Make sure we add an additional nibble to the command buffer if the number of nibbles is odd,
        // as we cannot send a standalone nibble.
        if self.output_buffer.len() % 2 == 1 {
            self.add_raw_command(Command::Flush)?;
        }

        self.send_buffer()?;

        while self.pending_in_bits != 0 {
            self.receive_buffer()?;
        }

        let iter = self
            .input_buffers
            .iter()
            .flat_map(|it| it.clone())
            .collect();

        self.input_buffers.clear();

        Ok(iter)
    }

    /// Writes a command one or multiple times into the raw buffer we send to the USB EP later
    /// or if the out buffer reaches a limit of `OUT_BUFFER_SIZE`.
    fn write_stream(
        &mut self,
        command: impl Into<Command>,
        repetitions: usize,
    ) -> Result<(), DebugProbeError> {
        let command = command.into();
        let mut repetitions = repetitions;
        tracing::trace!("add raw cmd {:?} reps={}", command, repetitions);

        // Make sure we send flush commands only once and not repeated (Could make the target unhapy).
        if command == Command::Flush {
            repetitions = 1;
        }

        // Send the actual command.
        self.add_raw_command(command)?;

        // We already sent the command once so we need to do one less repetition.
        repetitions -= 1;

        // Send repetitions as many times as required.
        // We only send 2 bits with each repetition command as per the protocol.
        while repetitions > 0 {
            self.add_raw_command(Command::Repetitions((repetitions & 3) as u8))?;
            repetitions >>= 2;
        }

        Ok(())
    }

    /// Adds a single command to the output buffer and writes it to the USB EP if the buffer reaches a limit of `OUT_BUFFER_SIZE`.
    fn add_raw_command(&mut self, command: impl Into<Command>) -> Result<(), DebugProbeError> {
        let command = command.into();
        self.output_buffer.push(command);

        // If we reach a maximal size of the output buffer, we flush.
        assert!(self.output_buffer.len() <= (OUT_BUFFER_SIZE * 2));
        if self.output_buffer.len() == (OUT_BUFFER_SIZE * 2) {
            self.send_buffer()?;
        }

        // Undocumented condition to flush buffer.
        // https://github.com/espressif/openocd-esp32/blob/a28f71785066722f49494e0d946fdc56966dcc0d/src/jtag/drivers/esp_usb_jtag.c#L367
        if self.output_buffer.len() % (OUT_EP_BUFFER_SIZE * 2) == 0 {
            if self.pending_in_bits > (64 + 4) * 8 {
                self.send_buffer()?;
            }
        }

        Ok(())
    }

    /// Sends the commands stored in the output buffer to the USB EP.
    fn send_buffer(&mut self) -> Result<(), DebugProbeError> {
        tracing::trace!("Send buffer: [{}]", self.output_buffer.len());

        let commands = self
            .output_buffer
            .chunks(2)
            .map(|chunk| {
                if chunk.len() == 2 {
                    let unibble: u8 = chunk[0].into();
                    let lnibble: u8 = chunk[1].into();
                    (unibble << 4) | lnibble
                } else {
                    chunk[0].into()
                }
            })
            .collect::<Vec<_>>();
        tracing::warn!("Writing {}byte ({}nibles) to usb endpoint", commands.len(), commands.len() * 2);
        let mut offset = 0;
        let mut total = 0;
        loop {
            let bytes = self
                .device_handle
                .write_bulk(self.ep_out, &commands[offset..], USB_TIMEOUT)
                .map_err(|e| DebugProbeError::Usb(Some(Box::new(e))))?;
            total += bytes;
            offset += bytes;

            if total == commands.len() {
                break;
            }
        }

        // assert_eq!(bytes, commands.len());
        // We only clear the output buffer on a successful transmission of all bytes.
        self.output_buffer.clear();

        // https://github.com/espressif/openocd-esp32/blob/a28f71785066722f49494e0d946fdc56966dcc0d/src/jtag/drivers/esp_usb_jtag.c#L345
        loop {
            if self.pending_in_bits > (64 + 4) * 8 {
                tracing::warn!("More than a buffer in pending: {}, trying to recieve until one left", self.pending_in_bits);
                self.receive_buffer()?;
            } else {
                tracing::warn!("Pending after: {}", self.pending_in_bits);
                break;
            }
        }

        // assert!(self.pending_in_bits < IN_EP_BUFFER_SIZE * 2, "{} pending bits exceeds the esp-serial-jtags internal buffer of {}bytes", self.pending_in_bits, IN_EP_BUFFER_SIZE * 2);

        Ok(())
    }

    /// Tries to receive pending in bits from the USB EP.
    fn receive_buffer(&mut self) -> Result<(), DebugProbeError> {
        let count = ((self.pending_in_bits + 7) / 8).min(IN_EP_BUFFER_SIZE);
        let mut incoming = vec![0; count];

        tracing::warn!("Receiving buffer, pending bits: {}", self.pending_in_bits);

        if count == 0 {
            return Ok(());
        }

        let mut offset = 0;
        let mut total = 0;
        loop {
            let read_bytes = self
                .device_handle
                .read_bulk(self.ep_in, &mut incoming[offset..], USB_TIMEOUT)
                .map_err(|e| {
                    tracing::warn!(
                        "Something went wrong in read_bulk {:?} when trying to read {}bytes - pending_in_bits: {}",
                        e,
                        count,
                        self.pending_in_bits,
                    );
                    tracing::warn!("Attempting to send data to recieve..");
                    DebugProbeError::Usb(Some(Box::new(e)))
                })?;
            total += read_bytes;
            offset += read_bytes;

            if read_bytes == 0 {
                tracing::warn!("Read 0 bytes from USB");
                return Ok(());
            }

            if total == count {
                break;
            } else {
                tracing::warn!("USB only recieved {} out of {} bytes", read_bytes, count);
            }

            tracing::trace!("Received {} bytes.", read_bytes);
        }

        let bits_in_buffer = self.pending_in_bits.min(total * 8);

        tracing::trace!("Read: {:?}, length = {}", incoming, bits_in_buffer);
        self.pending_in_bits -= bits_in_buffer;

        self.input_buffers
            .push(OwnedBitIter::new(&incoming, bits_in_buffer));

        Ok(())
    }
}

#[derive(PartialEq, Debug, Clone, Copy)]
pub(super) enum Command {
    Clock { cap: bool, tdi: bool, tms: bool },
    Reset(bool),
    Flush,
    // TODO: What is this?
    _Rsvd,
    Repetitions(u8),
}

impl From<Command> for u8 {
    fn from(command: Command) -> Self {
        match command {
            Command::Clock { cap, tdi, tms } => {
                (if cap { 4 } else { 0 } | if tms { 2 } else { 0 } | u8::from(tdi))
            }
            Command::Reset(srst) => 8 | u8::from(srst),
            Command::Flush => 0xA,
            Command::_Rsvd => 0xB,
            Command::Repetitions(repetitions) => 0xC + repetitions,
        }
    }
}

/// Try to read the serial number of a USB device.
fn read_serial_number<T: rusb::UsbContext>(
    device: &rusb::Device<T>,
    descriptor: &rusb::DeviceDescriptor,
) -> Result<String, rusb::Error> {
    let timeout = Duration::from_millis(100);

    let handle = device.open()?;
    let language = handle
        .read_languages(timeout)?
        .get(0)
        .cloned()
        .ok_or(rusb::Error::BadDescriptor)?;
    handle.read_serial_number_string(language, descriptor, timeout)
}

/// An iterator over a received bit stream.
#[derive(Clone)]
pub struct BitIter<'a> {
    buf: &'a [u8],
    next_bit: u8,
    bits_left: usize,
}

impl<'a> BitIter<'a> {
    pub(crate) fn new(buf: &'a [u8], total_bits: usize) -> Self {
        assert!(
            buf.len() * 8 >= total_bits,
            "cannot pull {} bits out of {} bytes",
            total_bits,
            buf.len()
        );

        Self {
            buf,
            next_bit: 0,
            bits_left: total_bits,
        }
    }

    /// Splits off another `BitIter` from `self`s current position that will return `count` bits.
    ///
    /// After this call, `self` will be advanced by `count` bits.
    pub fn split_off(&mut self, count: usize) -> BitIter<'a> {
        assert!(count <= self.bits_left);
        let other = Self {
            buf: self.buf,
            next_bit: self.next_bit,
            bits_left: count,
        };

        // Update self
        let next_byte = (count + self.next_bit as usize) / 8;
        self.next_bit = (count as u8 + self.next_bit) % 8;
        self.buf = &self.buf[next_byte..];
        self.bits_left -= count;
        other
    }
}

impl Iterator for BitIter<'_> {
    type Item = bool;

    fn next(&mut self) -> Option<bool> {
        if self.bits_left > 0 {
            let byte = self.buf.first().unwrap();
            let bit = byte & (1 << self.next_bit) != 0;
            if self.next_bit < 7 {
                self.next_bit += 1;
            } else {
                self.next_bit = 0;
                self.buf = &self.buf[1..];
            }

            self.bits_left -= 1;
            Some(bit)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.bits_left, Some(self.bits_left))
    }
}

impl ExactSizeIterator for BitIter<'_> {}

impl fmt::Debug for BitIter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self
            .clone()
            .map(|bit| if bit { '1' } else { '0' })
            .collect::<String>();
        write!(f, "BitIter({s})")
    }
}

use std::collections::VecDeque;

/// An iterator over a received bit stream.
#[derive(Clone)]
pub struct OwnedBitIter {
    buf: VecDeque<u8>,
    next_bit: u8,
    bits_left: usize,
}

impl OwnedBitIter {
    pub(crate) fn new(slice: &[u8], total_bits: usize) -> Self {
        assert!(
            slice.len() * 8 >= total_bits,
            "cannot pull {} bits out of {} bytes",
            total_bits,
            slice.len()
        );
        let mut buf = VecDeque::new();
        buf.extend(slice);
        Self {
            buf,
            next_bit: 0,
            bits_left: total_bits,
        }
    }

    pub fn into_bit_iter<'a>(&'a mut self) -> BitIter<'a> {
        self.buf.make_contiguous();
        BitIter::new(&self.buf.as_slices().0, self.bits_left)
    }
}

impl Iterator for OwnedBitIter {
    type Item = bool;

    fn next(&mut self) -> Option<bool> {
        if self.bits_left > 0 {
            let byte = self.buf.iter().next().unwrap();
            let bit = byte & (1 << self.next_bit) != 0;
            if self.next_bit < 7 {
                self.next_bit += 1;
            } else {
                self.next_bit = 0;
                self.buf.pop_front();
            }

            self.bits_left -= 1;
            Some(bit)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.bits_left, Some(self.bits_left))
    }
}

impl FromIterator<bool> for OwnedBitIter {
    fn from_iter<T>(iter: T) -> Self
    where
        T: IntoIterator<Item = bool>,
    {
        let iter = iter.into_iter();
        let (lower, upper) = iter.size_hint();
        let mut buf = VecDeque::with_capacity(upper.unwrap_or(lower));
        let mut total_bits = 0;
        let mut current_byte = 0;
        let mut bit_index: u8 = 0;
        for b in iter {
            if b {
                current_byte |= 1 << bit_index
            }
            if bit_index < 7 {
                bit_index += 1;
            } else {
                buf.push_back(current_byte);
                current_byte = 0;
                bit_index = 0;
            }
            total_bits += 1;
        }
        if bit_index > 0 {
            buf.push_back(current_byte);
        }

        OwnedBitIter {
            buf,
            next_bit: 0,
            bits_left: total_bits,
        }
    }
}

impl ExactSizeIterator for OwnedBitIter {}

impl fmt::Debug for OwnedBitIter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self
            .clone()
            .map(|bit| if bit { '1' } else { '0' })
            .collect::<String>();
        write!(f, "BitIter({s})")
    }
}

pub(super) fn is_espjtag_device<T: UsbContext>(device: &Device<T>) -> bool {
    // Check the VID/PID.
    if let Ok(descriptor) = device.device_descriptor() {
        descriptor.vendor_id() == USB_VID && descriptor.product_id() == USB_PID
    } else {
        false
    }
}

#[tracing::instrument(skip_all)]
pub fn list_espjtag_devices() -> Vec<DebugProbeInfo> {
    rusb::Context::new()
        .and_then(|context| context.devices())
        .map_or(vec![], |devices| {
            devices
                .iter()
                .filter(is_espjtag_device)
                .filter_map(|device| {
                    let descriptor = device.device_descriptor().ok()?;

                    let sn_str = match read_serial_number(&device, &descriptor) {
                        Ok(serial_number) => Some(serial_number),
                        Err(e) => {
                            // Reading the serial number can fail, e.g. if the driver for the probe
                            // is not installed. In this case we can still list the probe,
                            // just without serial number.
                            tracing::debug!(
                                "Failed to read serial number of device {:04x}:{:04x} : {}",
                                descriptor.vendor_id(),
                                descriptor.product_id(),
                                e
                            );
                            tracing::debug!("This might be happening because of a missing driver.");
                            None
                        }
                    };

                    Some(DebugProbeInfo::new(
                        "ESP JTAG".to_string(),
                        descriptor.vendor_id(),
                        descriptor.product_id(),
                        sn_str,
                        DebugProbeType::EspJtag,
                        None,
                    ))
                })
                .collect::<Vec<_>>()
        })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn owned_collect() {
        let one = [true, true, true, true, true, true, true, true, true];
        let two = [true, true, true, true, true, true, true, true, true];

        let bits = one.into_iter().chain(two.into_iter());

        let s = bits
            .clone()
            .map(|bit| if bit { '1' } else { '0' })
            .collect::<String>();

        let x: OwnedBitIter = bits.clone().collect();

        println!("Actual: {}, Owned: {:?} : {:?}", s, x, x.buf);

        assert!(bits.eq(x))
    }

    #[test]
    fn owned_split_off() {
        let one = [true, true, true, true, true, true, true, true, true];
        let two = [true, true, true, true, true, true, true, true, true];

        let bits = one.into_iter().chain(two.into_iter());

        let mut x: OwnedBitIter = bits.clone().collect();

        println!("Owned: {:?} : {:?} : {}", x, x.buf, x.bits_left);

        let a = x.split_off(9);

        assert!(one.into_iter().eq(a));
        assert!(two.into_iter().eq(x));
    }
}
