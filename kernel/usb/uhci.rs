use alloc::boxed::Box;

use collections::string::ToString;

use core::intrinsics::{volatile_load, volatile_store};
use core::{cmp, mem, ptr};

use scheduler::context::{self, Context};
use common::debug;
use common::event::MouseEvent;
use common::memory::{self, Memory};
use common::time::{self, Duration};

use drivers::pciconfig::PciConfig;
use drivers::pio::*;

use graphics::display::VBEMODEINFO;

use schemes::KScheme;

use sync::Intex;

pub struct Uhci {
    pub base: usize,
    pub irq: u8,
}

impl KScheme for Uhci {
    fn on_irq(&mut self, irq: u8) {
        if irq == self.irq {
            // d("UHCI IRQ\n");
        }
    }

    fn on_poll(&mut self) {
    }
}

#[repr(packed)]
#[derive(Copy, Clone, Debug, Default)]
struct Setup {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    len: u16,
}

#[repr(packed)]
#[derive(Copy, Clone, Debug, Default)]
struct Td {
    link_ptr: u32,
    ctrl_sts: u32,
    token: u32,
    buffer: u32, // reserved: [u32; 4]
}

#[repr(packed)]
#[derive(Copy, Clone, Debug, Default)]
struct Qh {
    head_ptr: u32,
    element_ptr: u32,
}

const DESC_DEV: u8 = 1;
#[repr(packed)]
#[derive(Copy, Clone, Debug, Default)]
struct DeviceDescriptor {
    length: u8,
    descriptor_type: u8,
    usb_version: u16,
    class: u8,
    sub_class: u8,
    protocol: u8,
    max_packet_size: u8,
    vendor: u16,
    product: u16,
    release: u16,
    manufacturer_string: u8,
    product_string: u8,
    serial_string: u8,
    configurations: u8,
}

const DESC_CFG: u8 = 2;
#[repr(packed)]
#[derive(Copy, Clone, Debug, Default)]
struct ConfigDescriptor {
    length: u8,
    descriptor_type: u8,
    total_length: u16,
    interfaces: u8,
    number: u8,
    string: u8,
    attributes: u8,
    max_power: u8,
}

const DESC_INT: u8 = 4;
#[repr(packed)]
#[derive(Copy, Clone, Debug, Default)]
struct InterfaceDescriptor {
    length: u8,
    descriptor_type: u8,
    number: u8,
    alternate: u8,
    endpoints: u8,
    class: u8,
    sub_class: u8,
    protocol: u8,
    string: u8,
}

const DESC_END: u8 = 5;
#[repr(packed)]
#[derive(Copy, Clone, Debug, Default)]
struct EndpointDescriptor {
    length: u8,
    descriptor_type: u8,
    address: u8,
    attributes: u8,
    max_packet_size: u16,
    interval: u8,
}

const DESC_HID: u8 = 0x21;
#[repr(packed)]
#[derive(Copy, Clone, Debug, Default)]
struct HIDDescriptor {
    length: u8,
    descriptor_type: u8,
    hid_version: u16,
    country_code: u8,
    descriptors: u8,
    sub_descriptor_type: u8,
    sub_descriptor_length: u16,
}

impl Uhci {
    pub unsafe fn new(mut pci: PciConfig) -> Box<Self> {
        pci.flag(4, 4, true); // Bus mastering

        let module = box Uhci {
            base: pci.read(0x20) as usize & 0xFFFFFFF0,
            irq: pci.read(0x3C) as u8 & 0xF,
        };

        module.init();

        return module;
    }

    unsafe fn set_address(&self, frame_list: *mut u32, address: u8) {
        let base = self.base as u16;
        let frnum = Pio16::new(base + 6);

        let mut in_td = Memory::<Td>::new(1).unwrap();
        in_td.store(0,
                    Td {
                        link_ptr: 1,
                        ctrl_sts: 1 << 23,
                        token: 0x7FF << 21 | 0x69,
                        buffer: 0,
                    });

        let mut setup = Memory::<Setup>::new(1).unwrap();
        setup.store(0,
                    Setup {
                        request_type: 0b00000000,
                        request: 5,
                        value: address as u16,
                        index: 0,
                        len: 0,
                    });

        let mut setup_td = Memory::<Td>::new(1).unwrap();
        setup_td.store(0,
                       Td {
                           link_ptr: in_td.address() as u32 | 4,
                           ctrl_sts: 1 << 23,
                           token: (mem::size_of::<Setup>() as u32 - 1) << 21 | 0x2D,
                           buffer: setup.address() as u32,
                       });

        let mut queue_head = Memory::<Qh>::new(1).unwrap();
        queue_head.store(0,
                         Qh {
                             head_ptr: 1,
                             element_ptr: setup_td.address() as u32,
                         });

        let frame = (frnum.read() + 2) & 0x3FF;
        ptr::write(frame_list.offset(frame as isize),
                   queue_head.address() as u32 | 2);

        loop {
            if setup_td.load(0).ctrl_sts & (1 << 23) == 0 {
                break;
            }
        }

        loop {
            if in_td.load(0).ctrl_sts & (1 << 23) == 0 {
                break;
            }
        }

        ptr::write(frame_list.offset(frame as isize), 1);
    }

    unsafe fn descriptor(&self,
                         frame_list: *mut u32,
                         address: u8,
                         descriptor_type: u8,
                         descriptor_index: u8,
                         descriptor_ptr: u32,
                         descriptor_len: u32) {
        let base = self.base as u16;
        let frnum = Pio16::new(base + 6);

        let mut out_td = Memory::<Td>::new(1).unwrap();
        out_td.store(0,
                     Td {
                         link_ptr: 1,
                         ctrl_sts: 1 << 23,
                         token: 0x7FF << 21 | (address as u32) << 8 | 0xE1,
                         buffer: 0,
                     });

        let mut in_td = Memory::<Td>::new(1).unwrap();
        in_td.store(0,
                    Td {
                        link_ptr: out_td.address() as u32 | 4,
                        ctrl_sts: 1 << 23,
                        token: (descriptor_len - 1) << 21 | (address as u32) << 8 | 0x69,
                        buffer: descriptor_ptr,
                    });

        let mut setup = Memory::<Setup>::new(1).unwrap();
        setup.store(0,
                    Setup {
                        request_type: 0b10000000,
                        request: 6,
                        value: (descriptor_type as u16) << 8 | (descriptor_index as u16),
                        index: 0,
                        len: descriptor_len as u16,
                    });

        let mut setup_td = Memory::<Td>::new(1).unwrap();
        setup_td.store(0,
                       Td {
                           link_ptr: in_td.address() as u32 | 4,
                           ctrl_sts: 1 << 23,
                           token: (mem::size_of::<Setup>() as u32 - 1) << 21 |
                                  (address as u32) << 8 | 0x2D,
                           buffer: setup.address() as u32,
                       });

        let mut queue_head = Memory::<Qh>::new(1).unwrap();
        queue_head.store(0,
                         Qh {
                             head_ptr: 1,
                             element_ptr: setup_td.address() as u32,
                         });

        let frame = (frnum.read() + 2) & 0x3FF;
        ptr::write(frame_list.offset(frame as isize),
                   queue_head.address() as u32 | 2);

        loop {
            if setup_td.load(0).ctrl_sts & (1 << 23) == 0 {
                break;
            }
        }

        loop {
            if in_td.load(0).ctrl_sts & (1 << 23) == 0 {
                break;
            }
        }

        loop {
            if out_td.load(0).ctrl_sts & (1 << 23) == 0 {
                break;
            }
        }

        ptr::write(frame_list.offset(frame as isize), 1);
    }

    unsafe fn device(&self, frame_list: *mut u32, address: u8) {
        self.set_address(frame_list, address);

        let desc_dev: *mut DeviceDescriptor = memory::alloc_type();
        ptr::write(desc_dev, DeviceDescriptor::default());
        self.descriptor(frame_list,
                        address,
                        DESC_DEV,
                        0,
                        desc_dev as u32,
                        mem::size_of_val(&*desc_dev) as u32);
        debugln!("{:#?}", *desc_dev);

        for configuration in 0..(*desc_dev).configurations {
            let desc_cfg_len = 1023;
            let desc_cfg_buf = memory::alloc(desc_cfg_len) as *mut u8;
            for i in 0..desc_cfg_len as isize {
                ptr::write(desc_cfg_buf.offset(i), 0);
            }
            self.descriptor(frame_list,
                            address,
                            DESC_CFG,
                            configuration,
                            desc_cfg_buf as u32,
                            desc_cfg_len as u32);

            let desc_cfg = ptr::read(desc_cfg_buf as *const ConfigDescriptor);
            debugln!("{:#?}", desc_cfg);

            let mut hid = false;

            let mut i = desc_cfg.length as isize;
            while i < desc_cfg.total_length as isize {
                let length = ptr::read(desc_cfg_buf.offset(i));
                let descriptor_type = ptr::read(desc_cfg_buf.offset(i + 1));
                match descriptor_type {
                    DESC_INT => {
                        let desc_int = ptr::read(desc_cfg_buf.offset(i) as *const InterfaceDescriptor);
                        debugln!("{:#?}", desc_int);
                    }
                    DESC_END => {
                        let desc_end = ptr::read(desc_cfg_buf.offset(i) as *const EndpointDescriptor);
                        debugln!("{:#?}", desc_end);

                        let endpoint = desc_end.address & 0xF;
                        let in_len = desc_end.max_packet_size as usize;

                        let base = self.base as u16;
                        let frnum = base + 0x6;

                        if hid {
                            Context::spawn("kuhci_hid".to_string(), box move || {
                                debugln!("Starting HID driver");

                                let in_ptr = memory::alloc(in_len) as *mut u8;
                                let in_td: *mut Td = memory::alloc_type();

                                loop {
                                    for i in 0..in_len as isize {
                                        volatile_store(in_ptr.offset(i), 0);
                                    }

                                    ptr::write(in_td,
                                               Td {
                                                   link_ptr: 1,
                                                   ctrl_sts: 1 << 25 | 1 << 23,
                                                   token: (in_len as u32 - 1) << 21 |
                                                          (endpoint as u32) << 15 |
                                                          (address as u32) << 8 |
                                                          0x69,
                                                   buffer: in_ptr as u32,
                                               });

                                    let frame = {
                                        let _intex = Intex::static_lock();

                                        let frame = (inw(frnum) + 2) & 0x3FF;
                                        volatile_store(frame_list.offset(frame as isize), in_td as u32);
                                        frame
                                    };

                                    loop {
                                        {
                                            let ctrl_sts = volatile_load(in_td).ctrl_sts;
                                            if ctrl_sts & (1 << 23) == 0 {
                                                break;
                                            }
                                        }

                                        context::context_switch(false);
                                    }

                                    volatile_store(frame_list.offset(frame as isize), 1);

                                    if volatile_load(in_td).ctrl_sts & 0x7FF > 0 {
                                       let buttons = ptr::read(in_ptr.offset(0) as *const u8) as usize;
                                       let x = ptr::read(in_ptr.offset(1) as *const u16) as usize;
                                       let y = ptr::read(in_ptr.offset(3) as *const u16) as usize;

                                       let mode_info = &*VBEMODEINFO;
                                       let mouse_x = (x * mode_info.xresolution as usize) / 32768;
                                       let mouse_y = (y * mode_info.yresolution as usize) / 32768;

                                       let mouse_event = MouseEvent {
                                           x: cmp::max(0, cmp::min(mode_info.xresolution as i32 - 1, mouse_x as i32)),
                                           y: cmp::max(0, cmp::min(mode_info.yresolution as i32 - 1, mouse_y as i32)),
                                           left_button: buttons & 1 == 1,
                                           middle_button: buttons & 4 == 4,
                                           right_button: buttons & 2 == 2,
                                       };
                                       ::env().events.lock().push_back(mouse_event.to_event());
                                    }

                                    Duration::new(0, 10 * time::NANOS_PER_MILLI).sleep();
                                }

                            // memory::unalloc(in_td as usize);
                            });
                        }
                    }
                    DESC_HID => {
                        let desc_hid = &*(desc_cfg_buf.offset(i) as *const HIDDescriptor);
                        debugln!("{:#?}", desc_hid);
                        hid = true;
                    }
                    _ => {
                        debug::d("Unknown Descriptor Length ");
                        debug::dd(length as usize);
                        debug::d(" Type ");
                        debug::dh(descriptor_type as usize);
                        debug::dl();
                    }
                }
                i += length as isize;
            }

            memory::unalloc(desc_cfg_buf as usize);
        }

        memory::unalloc(desc_dev as usize);
    }

    pub unsafe fn init(&self) {
        debug::d("UHCI on: ");
        debug::dh(self.base);
        debug::d(", IRQ: ");
        debug::dbh(self.irq);

        let base = self.base as u16;
        let usbcmd = base;
        let usbsts = base + 02;
        let usbintr = base + 0x4;
        let frnum = base + 0x6;
        let flbaseadd = base + 0x8;
        let portsc1 = base + 0x10;
        let portsc2 = base + 0x12;

        debug::d(" CMD ");
        debug::dh(inw(usbcmd) as usize);
        outw(usbcmd, 1 << 2 | 1 << 1);
        debug::d(" to ");
        debug::dh(inw(usbcmd) as usize);

        outw(usbcmd, 0);
        debug::d(" to ");
        debug::dh(inw(usbcmd) as usize);

        debug::d(" STS ");
        debug::dh(inw(usbsts) as usize);

        debug::d(" INTR ");
        debug::dh(inw(usbintr) as usize);

        debug::d(" FRNUM ");
        debug::dh(inw(frnum) as usize);
        outw(frnum, 0);
        debug::d(" to ");
        debug::dh(inw(frnum) as usize);

        debug::d(" FLBASEADD ");
        debug::dh(ind(flbaseadd) as usize);
        let frame_list = memory::alloc(1024 * 4) as *mut u32;
        for i in 0..1024 {
            ptr::write(frame_list.offset(i), 1);
        }
        outd(flbaseadd, frame_list as u32);
        debug::d(" to ");
        debug::dh(ind(flbaseadd) as usize);

        debug::d(" CMD ");
        debug::dh(inw(usbcmd) as usize);
        outw(usbcmd, 1);
        debug::d(" to ");
        debug::dh(inw(usbcmd) as usize);

        debug::dl();

        {
            debug::d(" PORTSC1 ");
            debug::dh(inw(portsc1) as usize);

            outw(portsc1, 1 << 9);
            debug::d(" to ");
            debug::dh(inw(portsc1) as usize);

            outw(portsc1, 0);
            debug::d(" to ");
            debug::dh(inw(portsc1) as usize);

            debug::dl();

            if inw(portsc1) & 1 == 1 {
                debug::d(" Device Found ");
                debug::dh(inw(portsc1) as usize);

                outw(portsc1, 4);
                debug::d(" to ");
                debug::dh(inw(portsc1) as usize);
                debug::dl();

                self.device(frame_list, 1);
            }
        }

        {
            debug::d(" PORTSC2 ");
            debug::dh(inw(portsc2) as usize);

            outw(portsc2, 1 << 9);
            debug::d(" to ");
            debug::dh(inw(portsc2) as usize);

            outw(portsc2, 0);
            debug::d(" to ");
            debug::dh(inw(portsc2) as usize);

            debug::dl();

            if inw(portsc2) & 1 == 1 {
                debug::d(" Device Found ");
                debug::dh(inw(portsc2) as usize);

                outw(portsc2, 4);
                debug::d(" to ");
                debug::dh(inw(portsc2) as usize);
                debug::dl();

                self.device(frame_list, 2);
            }
        }
    }
}
