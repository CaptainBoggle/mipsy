use std::rc::Rc;

use super::util::{inst_parts_to_string, inst_to_string, tip_header};
use crate::{Binary, InstSet, Register, Runtime, Safe, State, decompile::{self, Decompiled, decompile_inst_into_parts}, inst::ReadsRegisterType, runtime::state::{WRITE_MARKER_HI, WRITE_MARKER_LO}, util::{get_segment, Segment}};
use colored::Colorize;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RuntimeError {
    error: Error,
}

impl RuntimeError {
    pub fn new(error: Error) -> Self {
        Self { error }
    }

    pub fn error(&self) -> &Error {
        &self.error
    }

    pub fn show_error(
        &self,
        context: ErrorContext,
        source_code: Vec<(Rc<str>, Rc<str>)>,
        inst_set: &InstSet,
        binary: &Binary,
        runtime: &Runtime,
    ) {
        println!(
            "{}{} {}",
            "error".bright_red().bold(),
            ":".bold(),
            self.error.message(context, &source_code, inst_set, binary, runtime)
        );

        for tip in self.error.tips(&source_code, inst_set, binary, runtime) {
            println!("{} {}", tip_header(), tip);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ErrorContext {
    Binary,
    Interactive,
    Repl,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Error {
    UnknownInstruction { addr: u32 },
    Uninitialised { value: Uninitialised },
    UnalignedAccess { addr: u32, alignment_requirement: AlignmentRequirement },

    IntegerOverflow,
    DivisionByZero,

    SegmentationFault { addr: u32, access: SegmentationFaultAccessType },
    InvalidSyscall { syscall: i32, reason: InvalidSyscallReason },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Uninitialised {
    Byte { addr: u32 },
    Half { addr: u32 },
    Word { addr: u32 },
    Register { reg_num: u32 },
    Lo,
    Hi,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AlignmentRequirement {
    Half,
    Word,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum InvalidSyscallReason {
    Unimplemented, // Invalid becasue we don't have an implementation for it but it does exist
    Unknown,       // Invalid because it doesn't exist to begin with
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SegmentationFaultAccessType {
    Read,
    Write,
    Execute,
}

impl Error {
    pub fn message(
        &self,
        context: ErrorContext,
        source_code: &[(Rc<str>, Rc<str>)],
        inst_set: &InstSet,
        binary: &Binary,
        runtime: &Runtime,
    ) -> String {
        match self {
            Error::UnknownInstruction { addr } => {
                let message = "could not find instruction at";
                let zero_x = "0x".yellow();

                format!("{} {}{:08x}\n", message, zero_x, addr)
            }

            Error::Uninitialised { value } => {
                let (name, last_mod) = match value {
                    Uninitialised::Byte { addr }
                    | Uninitialised::Half { addr }
                    | Uninitialised::Word { addr } => {
                        let size = match value {
                            Uninitialised::Byte { addr: _ } => "byte",
                            Uninitialised::Half { addr: _ } => "half",
                            Uninitialised::Word { addr: _ } => "word",
                            _ => unreachable!(),
                        };

                        let message = "is uninitialised";
                        let zero_x = "0x".yellow();
                        return format!("{} at {}{:08x} {}", size, zero_x, addr, message);
                    }

                    Uninitialised::Register { reg_num } => {
                        let name = Register::from_u32(*reg_num).unwrap().to_lower_str();
                        let last_mod = get_last_mod(runtime, *reg_num);

                        (name, last_mod)
                    }

                    Uninitialised::Lo => {
                        let name = "lo";
                        let last_mod = get_last_mod(runtime, WRITE_MARKER_LO);

                        (name, last_mod)
                    }

                    Uninitialised::Hi => {
                        let name = "hi";
                        let last_mod = get_last_mod(runtime, WRITE_MARKER_HI);

                        (name, last_mod)
                    }
                };

                let mut error = String::new();
                error.push_str("your program tried to read an uninitialised register\n");

                let state = runtime.timeline().state();
                let inst = state.read_mem_word(state.pc()).unwrap();
                let decompiled =
                    decompile::decompile_inst_into_parts(binary, inst_set, inst, state.pc());

                if let ErrorContext::Binary | ErrorContext::Interactive = context {
                    error.push_str("the instruction that failed was:\n");
                    error.push_str(&inst_parts_to_string(
                        &decompiled,
                        source_code,
                        binary,
                        false,
                        false,
                    ));
                    error.push('\n');
                }

                if decompiled.location.is_none() || !binary.line_numbers.contains_key(&(state.pc() + 4)) {
                    if let Some(real_inst_parts) = get_real_instruction_start(state, binary, inst_set, state.pc()) {

                        let (file_tag, line_num) = real_inst_parts.location.unwrap();
                        let mut file = None;

                        for (src_tag, src_file) in source_code {
                            if *file_tag == **src_tag {
                                file = Some(src_file);
                                break;
                            }
                        }

                        if let Some(file) = file {
                            if let Some(line) = file.lines().nth((line_num - 1) as usize) {
                                error.push_str(&format!(
                                    "{}\n{} this instruction was generated from your pseudo-instruction:\n",
                                    ">".red(),
                                    "|".red(),
                                ));
                                
                                error.push_str(&format!(
                                    "{} {} {}\n",
                                    "|".red(),
                                    line_num.to_string().yellow().bold(),
                                    line.bold(),
                                ));

                                error.push_str(&format!(
                                    "{} which was expanded into the following {} native instructions:\n",
                                    "|".red(),
                                    (state.pc() + 4 - real_inst_parts.addr) / 4,
                                ));

                                for addr in (real_inst_parts.addr..try_find_pseudo_expansion_end(binary, runtime.timeline().state(), real_inst_parts.addr)).step_by(4) {
                                    let inst = state.read_mem_word(addr).unwrap();

                                    let failed = addr == state.pc();

                                    error.push_str(&format!(
                                        "  {} {}{}\n",
                                        if failed { ">".green() } else { ">".bright_black() },
                                        inst_to_string(inst, addr, source_code, binary, inst_set, failed, false),
                                        if failed {
                                            "  <-- this instruction failed"
                                                .bright_blue()
                                                .to_string()
                                        } else {
                                            String::new()
                                        },
                                    ));
                                }
                            }
                        }
                    }
                }

                error.push_str(&format!(
                    "\nthis happened because {}{} was uninitialised.\n",
                    "$".yellow(),
                    name.bold()
                ));

                if let Some((last_index, last_mod)) = last_mod {
                    error.push_str(&format!(
                        "{}\n{} the instruction that caused {}{} to become uninitialised was:\n",
                        ">".red(),
                        "|".red(),
                        "$".yellow(),
                        name.bold()
                    ));

                    let last_inst = last_mod.read_mem_word(last_mod.pc() - 4).unwrap();
                    let last_inst_parts = decompile_inst_into_parts(binary, inst_set, last_inst, last_mod.pc() - 4);

                    error.push_str(&format!(
                        "{} {}\n",
                        "|".red(),
                        inst_parts_to_string(&last_inst_parts, source_code, binary, false, false),
                    ));

                    if let Some(runtime_meta) = last_inst_parts.runtime_meta {
                        if let Some(inst_sig) = last_inst_parts.inst_sig {
                            if !runtime_meta.reads().is_empty() {
                                error.push_str(&format!(
                                    "{} {}:\n",
                                    "|".red(),
                                    "where".bold(),
                                ));

                                let rs =    (last_inst >> 21) & 0x1F;
                                let rt =    (last_inst >> 16) & 0x1F;

                                for read in runtime_meta.reads() {
                                    for (index, argument) in inst_sig.format().iter().enumerate() {
                                        if read.eq_argument_type(argument) {
                                            let value = match read {
                                                ReadsRegisterType::Rs | ReadsRegisterType::OffRs => last_mod.read_register_uninit(rs),
                                                ReadsRegisterType::Rt | ReadsRegisterType::OffRt => last_mod.read_register_uninit(rt),
                                            };

                                            let name = match read {
                                                ReadsRegisterType::Rs | ReadsRegisterType::Rt => {
                                                    format!("{}{}", "$".yellow(), last_inst_parts.arguments[index][1..].bold())
                                                },
                                                ReadsRegisterType::OffRs | ReadsRegisterType::OffRt => {
                                                    format!("{}{}", "$".yellow(), last_inst_parts.arguments[index].split_once('(').unwrap().1.split_once(')').unwrap().0[1..].bold())
                                                }
                                            };

                                            error.push_str(&format!(
                                                "{}  {} = {}\n",
                                                "|".red(),
                                                name,
                                                match value {
                                                    Safe::Valid(value)  => format!("0x{:08x}", value),
                                                    Safe::Uninitialised => String::from("uninitialised"),
                                                },
                                            ));

                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    let last_next_inst_parts = decompile_inst_into_parts(binary, inst_set, last_inst, last_mod.pc());
                    if last_inst_parts.location.is_none() || last_next_inst_parts.location.is_none() {
                        if let Some(real_inst_parts) = get_real_instruction_start(last_mod, binary, inst_set, last_mod.pc() - 4) {

                            let (file_tag, line_num) = real_inst_parts.location.unwrap();
                            let mut file = None;

                            for (src_tag, src_file) in source_code {
                                if *file_tag == **src_tag {
                                    file = Some(src_file);
                                    break;
                                }
                            }

                            if let Some(file) = file {
                                if let Some(line) = file.lines().nth((line_num - 1) as usize) {
                                    error.push_str(&format!(
                                        "{}\n  {} this instruction was generated from your pseudo-instruction:\n",
                                        ">=>".red(),
                                        "|".red(),
                                    ));
                                    
                                    error.push_str(&format!(
                                        "  {} {} {}\n",
                                        "|".red(),
                                        line_num.to_string().yellow().bold(),
                                        line.bold(),
                                    ));

                                    error.push_str(&format!(
                                        "  {} which was expanded into the following {} native instructions:\n",
                                        "|".red(),
                                        (last_mod.pc() - real_inst_parts.addr) / 4,
                                    ));

                                    for addr in (real_inst_parts.addr..try_find_pseudo_expansion_end(binary, runtime.timeline().state(), real_inst_parts.addr)).step_by(4) {
                                        let inst = last_mod.read_mem_word(addr).unwrap();

                                        let failed = addr == last_mod.pc() - 4;

                                        error.push_str(&format!(
                                            "  {} {}{}\n",
                                            if failed { ">".green() } else { ">".bright_black() },
                                            inst_to_string(inst, addr, source_code, binary, inst_set, failed, false),
                                            if failed {
                                                format!(
                                                    "  <-- this instruction caused {}{} to become uninitialised",                   
                                                    "$".yellow(),
                                                    name.white().bold(),
                                                )
                                                    .bright_blue()
                                                    .to_string()
                                            } else {
                                                String::new()
                                            },
                                        ));
                                    }
                                }
                            }
                        }
                    }

                    if let ErrorContext::Interactive = context {
                        let distance = runtime.timeline().timeline_len() - last_index;
                        error.push_str(&format!(
                            "{}\n{} to get back there, use `{} {}`\n",
                            ">".red(),
                            "|".red(),
                            "back".bold(),
                            distance.to_string().bold()
                        ));
                    }
                } else {
                    (error.push_str(&format!(
                        "{} note: {}{} was {} initialised\n",
                        "|".red(),
                        "$".yellow(),
                        name.bold(),
                        "never".bold()
                    )));
                }

                error.push('\n');

                error
            }

            Error::UnalignedAccess { addr, alignment_requirement } => {
                let mut error = String::new();

                error.push_str("unaligned access\n");

                let state = runtime.timeline().state();
                let inst = state.read_mem_word(state.pc()).unwrap();
                let decompiled = decompile::decompile_inst_into_parts(binary, inst_set, inst, state.pc());

                if let ErrorContext::Binary | ErrorContext::Interactive = context {
                    error.push_str("\nthe instruction that failed was:\n");
                    error.push_str(&inst_parts_to_string(
                        &decompiled,
                        source_code,
                        binary,
                        false,
                        false,
                    ));
                    error.push('\n');
                }

                if decompiled.location.is_none() || !binary.line_numbers.contains_key(&(state.pc() + 4)) {
                    if let Some(real_inst_parts) = get_real_instruction_start(state, binary, inst_set, state.pc()) {

                        let (file_tag, line_num) = real_inst_parts.location.unwrap();
                        let mut file = None;

                        for (src_tag, src_file) in source_code {
                            if *file_tag == **src_tag {
                                file = Some(src_file);
                                break;
                            }
                        }

                        if let Some(file) = file {
                            if let Some(line) = file.lines().nth((line_num - 1) as usize) {
                                error.push_str(&format!(
                                    "{}\n{} this instruction was generated from your pseudo-instruction:\n",
                                    ">".red(),
                                    "|".red(),
                                ));
                                
                                error.push_str(&format!(
                                    "{} {} {}\n",
                                    "|".red(),
                                    line_num.to_string().yellow().bold(),
                                    line.bold(),
                                ));

                                error.push_str(&format!(
                                    "{} which was expanded into the following {} native instructions:\n",
                                    "|".red(),
                                    (state.pc() + 4 - real_inst_parts.addr) / 4,
                                ));

                                for addr in (real_inst_parts.addr..try_find_pseudo_expansion_end(binary, runtime.timeline().state(), real_inst_parts.addr)).step_by(4) {
                                    let inst = state.read_mem_word(addr).unwrap();

                                    let failed = addr == state.pc();

                                    error.push_str(&format!(
                                        "{} {} {}{}\n",
                                        "|".red(),
                                        if failed { ">".green() } else { ">".bright_black() },
                                        inst_to_string(inst, addr, source_code, binary, inst_set, failed, false),
                                        if failed {
                                            "  <-- this instruction failed"
                                                .bright_blue()
                                                .to_string()
                                        } else {
                                            String::new()
                                        },
                                    ));
                                }
                            }
                        }
                    }
                }

                error.push('\n');

                let alignment_bytes = match alignment_requirement {
                    AlignmentRequirement::Half => 2,
                    AlignmentRequirement::Word => 4,
                };

                let argument = {
                    let unformatted = &decompiled.arguments[1];

                    if unformatted.contains('(') {
                        format!(
                            "{}({}{})",
                            unformatted.split_once('(').unwrap().0,
                            "$".yellow(),
                            unformatted.split_once('$').unwrap().1
                                .split_once(')').unwrap().0
                                .bold(),
                        )
                    } else {
                        format!(
                            "{}{}",
                            "$".yellow(),
                            unformatted.split_once('$').unwrap().1
                                .bold(),
                        )
                    }
                };

                error.push_str(
                    &format!(
                        "this happened because `{}` requires {} alignment,\n but the address ({} = {}) is not divisible by {}\n",
                        decompiled.inst_name.unwrap().bold(),
                        format!("{}-byte", alignment_bytes).bold(),
                        argument,
                        format!("0x{:08x}", *addr).bold(),
                        alignment_bytes.to_string().bold(),
                    )
                );

                error
            }

            Error::IntegerOverflow => {
                let mut error = String::new();
                error.push_str("integer overflow\n");

                let state = runtime.timeline().state();
                let inst = state.read_mem_word(state.pc()).unwrap();
                let decompiled =
                    decompile::decompile_inst_into_parts(binary, inst_set, inst, state.pc());

                if let ErrorContext::Binary | ErrorContext::Interactive = context {
                    error.push_str("\nthe instruction that failed was:\n");
                    error.push_str(&inst_parts_to_string(
                        &decompiled,
                        source_code,
                        binary,
                        false,
                        false,
                    ));
                    error.push('\n');
                }

                let rs = (inst >> 21) & 0x1F;
                let rs_value = runtime.timeline().state().read_register(rs).unwrap();
                error.push_str("values:\n");
                error.push_str(&format!(
                    " - {}{} = {}\n",
                    "$".yellow(),
                    Register::from_u32(rs).unwrap().to_lower_str().bold(),
                    rs_value,
                ));

                let value = if let Ok(imm) = decompiled.arguments[2].parse::<i16>() {
                    imm as i32
                } else {
                    let rt = (inst >> 16) & 0x1F;
                    let value = runtime.timeline().state().read_register(rt).unwrap();

                    error.push_str(&format!(
                        " - {}{} = {}\n",
                        "$".yellow(),
                        Register::from_u32(rt).unwrap().to_lower_str().bold(),
                        value
                    ));

                    value
                };

                let adding = decompiled
                    .inst_name
                    .as_ref()
                    .map(|name| name.contains("add"))
                    .unwrap_or(false);

                let symbol = if adding { "+" } else { "-" };

                error.push_str(&format!("this happened because `{}` {} `{}` overflows past the limits of a 32-bit number\n\n", rs_value, symbol, value));

                error
            }

            Error::DivisionByZero => {
                let mut error = String::new();

                error.push_str("division by zero\n");

                let state = runtime.timeline().state();
                let inst = state.read_mem_word(state.pc()).unwrap();
                let decompiled =
                    decompile::decompile_inst_into_parts(binary, inst_set, inst, state.pc());

                if let ErrorContext::Binary | ErrorContext::Interactive = context {
                    error.push_str("\nthe instruction that failed was:\n");
                    error.push_str(&inst_parts_to_string(
                        &decompiled,
                        source_code,
                        binary,
                        false,
                        false,
                    ));
                    error.push('\n');
                }

                let rs = (inst >> 21) & 0x1F;
                let rt = (inst >> 16) & 0x1F;

                error.push_str("\nvalues:\n");

                error.push_str(&format!(
                    " - {}{} = {}\n",
                    "$".yellow(),
                    Register::from_u32(rs).unwrap().to_lower_str().bold(),
                    runtime.timeline().state().read_register(rs).unwrap()
                ));

                error.push_str(&format!(
                    " - {}{} = {}\n",
                    "$".yellow(),
                    Register::from_u32(rt).unwrap().to_lower_str().bold(),
                    runtime.timeline().state().read_register(rt).unwrap()
                ));

                error
            }

            &Error::SegmentationFault { addr, access } => {
                let mut error = String::new();

                error.push_str("segmentation fault\n");

                match access {
                    SegmentationFaultAccessType::Read => {
                        error.push_str(&format!("\nthis happened because you tried to {} from\n", "read".yellow()));
                        error.push_str(&format!("the address `{}{}`, which is not a valid address to read from\n", "0x".bold(), format!("{:08x}", addr).bold()));
                    }
                    SegmentationFaultAccessType::Write => {
                        error.push_str(&format!("\nthis happened because you tried to {} to\n", "write".yellow()));
                        error.push_str(&format!("the address `{}{}`, which is not a valid address to write to\n", "0x".bold(), format!("{:08x}", addr).bold()));
                    }
                    SegmentationFaultAccessType::Execute => {
                        error.push_str(&format!("\nthis happened because you tried to {}\n", "execute".yellow()));
                        error.push_str(&format!("the address `{}{}`, which is not a valid address to execute\n", "0x".bold(), format!("{:08x}", addr).bold()));
                    }
                }

                let state = runtime.timeline().state();
                let prev_state = runtime.timeline().prev_state();

                if access == SegmentationFaultAccessType::Read || access == SegmentationFaultAccessType::Write {
                    // Current instruction is in a TEXT segment, so we are probaly reading or writing incorrectly
                    let inst = state.read_mem_word(state.pc()).unwrap();
                    let decompiled = decompile::decompile_inst_into_parts(binary, inst_set, inst, state.pc());

                    if let ErrorContext::Binary | ErrorContext::Interactive = context {
                        error.push_str("\nthe instruction that failed was:\n");
                        error.push_str(&inst_parts_to_string(
                            &decompiled,
                            source_code,
                            binary,
                            false,
                            false,
                        ));
                        error.push('\n');
                    }

                    if decompiled.location.is_none() || !binary.line_numbers.contains_key(&(state.pc() + 4)) {
                        if let Some(real_inst_parts) = get_real_instruction_start(state, binary, inst_set, state.pc()) {

                            let (file_tag, line_num) = real_inst_parts.location.unwrap();
                            let mut file = None;

                            for (src_tag, src_file) in source_code {
                                if *file_tag == **src_tag {
                                    file = Some(src_file);
                                    break;
                                }
                            }

                            if let Some(file) = file {
                                if let Some(line) = file.lines().nth((line_num - 1) as usize) {
                                    error.push_str(&format!(
                                        "{}\n{} this instruction was generated from your pseudo-instruction:\n",
                                        ">".red(),
                                        "|".red(),
                                    ));
                                    
                                    error.push_str(&format!(
                                        "{} {} {}\n",
                                        "|".red(),
                                        line_num.to_string().yellow().bold(),
                                        line.bold(),
                                    ));

                                    error.push_str(&format!(
                                        "{} which was expanded into the following {} native instructions:\n",
                                        "|".red(),
                                        (state.pc() + 4 - real_inst_parts.addr) / 4,
                                    ));

                                    for addr in (real_inst_parts.addr..try_find_pseudo_expansion_end(binary, runtime.timeline().state(), real_inst_parts.addr)).step_by(4) {
                                        let inst = state.read_mem_word(addr).unwrap();

                                        let failed = addr == state.pc();

                                        error.push_str(&format!(
                                            "{} {} {}{}\n",
                                            "|".red(),
                                            if failed { ">".green() } else { ">".bright_black() },
                                            inst_to_string(inst, addr, source_code, binary, inst_set, failed, false),
                                            if failed {
                                                "  <-- this instruction failed"
                                                    .bright_blue()
                                                    .to_string()
                                            } else {
                                                String::new()
                                            },
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                else if get_segment(prev_state.unwrap().pc()) == Segment::Text || get_segment(prev_state.unwrap().pc()) == Segment::KText {
                    // Current instruction is not in a TEXT segment, so we are probably executing incorrectly
                    // But we had to get here somehow, so the previuos instruction should be valid.
                    let state = prev_state.unwrap();
                    let inst = state.read_mem_word(state.pc()).unwrap();
                    let decompiled = decompile::decompile_inst_into_parts(binary, inst_set, inst, state.pc());

                    if let ErrorContext::Binary | ErrorContext::Interactive = context {
                        error.push_str("\nthe instruction that got us here was:\n");
                        error.push_str(&inst_parts_to_string(
                            &decompiled,
                            source_code,
                            binary,
                            false,
                            false,
                        ));
                        error.push('\n');
                    }

                    if decompiled.location.is_none() || !binary.line_numbers.contains_key(&(state.pc() + 4)) {
                        if let Some(real_inst_parts) = get_real_instruction_start(state, binary, inst_set, state.pc()) {

                            let (file_tag, line_num) = real_inst_parts.location.unwrap();
                            let mut file = None;

                            for (src_tag, src_file) in source_code {
                                if *file_tag == **src_tag {
                                    file = Some(src_file);
                                    break;
                                }
                            }

                            if let Some(file) = file {
                                if let Some(line) = file.lines().nth((line_num - 1) as usize) {
                                    error.push_str(&format!(
                                        "{}\n{} this instruction was generated from your pseudo-instruction:\n",
                                        ">".red(),
                                        "|".red(),
                                    ));
                                    
                                    error.push_str(&format!(
                                        "{} {} {}\n",
                                        "|".red(),
                                        line_num.to_string().yellow().bold(),
                                        line.bold(),
                                    ));

                                    error.push_str(&format!(
                                        "{} which was expanded into the following {} native instructions:\n",
                                        "|".red(),
                                        (state.pc() + 4 - real_inst_parts.addr) / 4,
                                    ));

                                    for addr in (real_inst_parts.addr..try_find_pseudo_expansion_end(binary, runtime.timeline().state(), real_inst_parts.addr)).step_by(4) {
                                        let inst = state.read_mem_word(addr).unwrap();

                                        let failed = addr == state.pc();

                                        error.push_str(&format!(
                                            "{} {} {}{}\n",
                                            "|".red(),
                                            if failed { ">".green() } else { ">".bright_black() },
                                            inst_to_string(inst, addr, source_code, binary, inst_set, failed, false),
                                            if failed {
                                                "  <-- this instruction failed"
                                                    .bright_blue()
                                                    .to_string()
                                            } else {
                                                String::new()
                                            },
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }

                error.push('\n');

                error

            }

            &Error::InvalidSyscall { syscall , reason } => {
                // This error is triggered when $v0 is not a known value when `syscall` is used
                // This error triggers after $v0 is checked for being uninitialize, so we can can assume that $v0 is an integer value

                let mut error = String::new();

                error.push_str("Invalid Syscall\n");

                match reason {
                    InvalidSyscallReason::Unimplemented => {
                        // $v0 is a valid value, but mipsy doesn't implement the operation
                        error.push_str(&format!("\nthe syscall number `{}` is not implemented.\n", syscall.to_string().bold()));
                    }
                    InvalidSyscallReason::Unknown => {
                        // $v0 is an invalid value, mips doesn't define this syscall
                        error.push_str(&format!("\nthe syscall number `{}` is not valid.\n", syscall.to_string().bold()));
                    }
                }

                let last_mod = get_last_mod(runtime, Register::V0.to_u32());
                let state = runtime.timeline().state();
                let inst = state.read_mem_word(state.pc()).unwrap();
                let decompiled = decompile::decompile_inst_into_parts(binary, inst_set, inst, state.pc());

                // This instruction should always be a `syscall`
                // This is mainly used to print the line number of the instruction
                if let ErrorContext::Binary | ErrorContext::Interactive = context {
                    error.push_str("\nthe instruction that failed was:\n");
                    error.push_str(&inst_parts_to_string(
                        &decompiled,
                        source_code,
                        binary,
                        false,
                        false,
                    ));
                    error.push('\n');
                }

                if decompiled.location.is_none() || !binary.line_numbers.contains_key(&(state.pc() + 4)) {
                    if let Some(real_inst_parts) = get_real_instruction_start(state, binary, inst_set, state.pc()) {

                        let (file_tag, line_num) = real_inst_parts.location.unwrap();
                        let mut file = None;

                        for (src_tag, src_file) in source_code {
                            if *file_tag == **src_tag {
                                file = Some(src_file);
                                break;
                            }
                        }

                        if let Some(file) = file {
                            if let Some(line) = file.lines().nth((line_num - 1) as usize) {
                                error.push_str(&format!(
                                    "{}\n{} this instruction was generated from your pseudo-instruction:\n",
                                    ">".red(),
                                    "|".red(),
                                ));
                                
                                error.push_str(&format!(
                                    "{} {} {}\n",
                                    "|".red(),
                                    line_num.to_string().yellow().bold(),
                                    line.bold(),
                                ));

                                error.push_str(&format!(
                                    "{} which was expanded into the following {} native instructions:\n",
                                    "|".red(),
                                    (state.pc() + 4 - real_inst_parts.addr) / 4,
                                ));

                                for addr in (real_inst_parts.addr..try_find_pseudo_expansion_end(binary, runtime.timeline().state(), real_inst_parts.addr)).step_by(4) {
                                    let inst = state.read_mem_word(addr).unwrap();

                                    let failed = addr == state.pc();

                                    error.push_str(&format!(
                                        "  {} {}{}\n",
                                        if failed { ">".green() } else { ">".bright_black() },
                                        inst_to_string(inst, addr, source_code, binary, inst_set, failed, false),
                                        if failed {
                                            "  <-- this instruction failed"
                                                .bright_blue()
                                                .to_string()
                                        } else {
                                            String::new()
                                        },
                                    ));
                                }
                            }
                        }
                    }
                }

                error.push_str(&format!(
                    "\nthis happened because {}{} was `{}`.\n",
                    "$".yellow(),
                    "v0".white().bold(),
                    syscall.to_string().bold(),
                ));

                if let Some((last_index, last_mod)) = last_mod {
                    error.push_str(&format!(
                        "{}\n{} the instruction that caused {}{} to become `{}` was:\n",
                        ">".red(),
                        "|".red(),
                        "$".yellow(),
                        "v0".white().bold(),
                        syscall.to_string().bold(),
                    ));

                    let last_inst = last_mod.read_mem_word(last_mod.pc() - 4).unwrap();
                    let last_inst_parts = decompile_inst_into_parts(binary, inst_set, last_inst, last_mod.pc() - 4);

                    error.push_str(&format!(
                        "{} {}\n",
                        "|".red(),
                        inst_parts_to_string(&last_inst_parts, source_code, binary, false, false),
                    ));

                    if let Some(runtime_meta) = last_inst_parts.runtime_meta {
                        if let Some(inst_sig) = last_inst_parts.inst_sig {
                            if !runtime_meta.reads().is_empty() {
                                error.push_str(&format!(
                                    "{} {}:\n",
                                    "|".red(),
                                    "where".bold(),
                                ));

                                let rs =    (last_inst >> 21) & 0x1F;
                                let rt =    (last_inst >> 16) & 0x1F;

                                for read in runtime_meta.reads() {
                                    for (index, argument) in inst_sig.format().iter().enumerate() {
                                        if read.eq_argument_type(argument) {
                                            let value = match read {
                                                ReadsRegisterType::Rs | ReadsRegisterType::OffRs => state.read_register_uninit(rs),
                                                ReadsRegisterType::Rt | ReadsRegisterType::OffRt => state.read_register_uninit(rt),
                                            };

                                            let name = match read {
                                                ReadsRegisterType::Rs | ReadsRegisterType::Rt => {
                                                    format!("{}{}", "$".yellow(), last_inst_parts.arguments[index][1..].bold())
                                                },
                                                ReadsRegisterType::OffRs | ReadsRegisterType::OffRt => {
                                                    format!("{}{}", "$".yellow(), last_inst_parts.arguments[index].split_once('(').unwrap().1.split_once(')').unwrap().0[1..].bold())
                                                }
                                            };

                                            error.push_str(&format!(
                                                "{}  {} = {}\n",
                                                "|".red(),
                                                name,
                                                match value {
                                                    Safe::Valid(value)  => format!("0x{:08x}", value),
                                                    Safe::Uninitialised => String::from("uninitialised"),
                                                },
                                            ));

                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    let last_next_inst_parts = decompile_inst_into_parts(binary, inst_set, last_inst, last_mod.pc());
                    if last_inst_parts.location.is_none() || last_next_inst_parts.location.is_none() {
                        if let Some(real_inst_parts) = get_real_instruction_start(last_mod, binary, inst_set, last_mod.pc() - 4) {

                            let (file_tag, line_num) = real_inst_parts.location.unwrap();
                            let mut file = None;

                            for (src_tag, src_file) in source_code {
                                if *file_tag == **src_tag {
                                    file = Some(src_file);
                                    break;
                                }
                            }

                            if let Some(file) = file {
                                if let Some(line) = file.lines().nth((line_num - 1) as usize) {
                                    error.push_str(&format!(
                                        "{}\n  {} this instruction was generated from your pseudo-instruction:\n",
                                        ">=>".red(),
                                        "|".red(),
                                    ));
                                    
                                    // TODO: It would be nice to syntax highlight the instruction printed here
                                    error.push_str(&format!(
                                        "  {} {} {}\n",
                                        "|".red(),
                                        line_num.to_string().yellow().bold(),
                                        line.bold(),
                                    ));

                                    error.push_str(&format!(
                                        "  {} which was expanded into the following {} native instructions:\n",
                                        "|".red(),
                                        (last_mod.pc() - real_inst_parts.addr) / 4,
                                    ));

                                    for addr in (real_inst_parts.addr..try_find_pseudo_expansion_end(binary, runtime.timeline().state(), real_inst_parts.addr)).step_by(4) {
                                        let inst = last_mod.read_mem_word(addr).unwrap();

                                        let failed = addr == last_mod.pc() - 4;

                                        error.push_str(&format!(
                                            "  {} {}{}\n",
                                            if failed { ">".green() } else { ">".bright_black() },
                                            inst_to_string(inst, addr, source_code, binary, inst_set, failed, false),
                                            if failed {
                                                format!(
                                                    "  <-- this instruction caused {}{} to become `{}`",
                                                    "$".yellow(),
                                                    "v0".white().bold(),
                                                    syscall.to_string().white().bold(),
                                                )
                                                    .bright_blue()
                                                    .to_string()
                                            } else {
                                                String::new()
                                            },
                                        ));
                                    }
                                }
                            }
                        }
                    }

                    if let ErrorContext::Interactive = context {
                        let distance = runtime.timeline().timeline_len() - last_index;
                        error.push_str(&format!(
                            "{}\n{} to get back there, use `{} {}`\n",
                            ">".red(),
                            "|".red(),
                            "back".bold(),
                            distance.to_string().bold()
                        ));
                    }
                }

                error.push('\n');

                error
            }
        }
    }

    pub fn tips(&self, source_code: &[(Rc<str>, Rc<str>)], inst_set: &InstSet, binary: &Binary, runtime: &Runtime) -> Vec<String> {
        match self {
            Error::UnknownInstruction { addr } => {
                if let Some(prev_state) = runtime.timeline().prev_state() {
                    // we got here in standard sequence,
                    //   usually describing a missing return

                    if prev_state.pc() + 4 == *addr {
                        let jr     = "jr".bold();
                        let dollar = "$".yellow();
                        let ra     = "ra".bold();

                        let did_you_forget = format!("did you forget to use `{jr} {dollar}{ra}`?");

                        if let Ok(inst) = prev_state.read_mem_word(prev_state.pc()) {
                            let decompiled = decompile::decompile_inst_into_parts(binary, inst_set, inst, prev_state.pc());
                            let inst_str = inst_parts_to_string(&decompiled, source_code, binary, false, true);

                            vec![
                                format!("the last instruction to execute was:\n{inst_str}\n"),
                                did_you_forget,
                            ]
                        } else {
                            // how the hell did we get here?
                            // maybe we can analytic these cases in future..
                            vec![
                                did_you_forget,
                            ]
                        }
                    } else if let Ok(inst) = prev_state.read_mem_word(prev_state.pc()) {
                        // we got here from some kind of botched jump,
                        //   how peculiar!

                        let decompiled = decompile::decompile_inst_into_parts(binary, inst_set, inst, prev_state.pc());
                        let inst_str = inst_parts_to_string(&decompiled, source_code, binary, false, true);

                        let back = "back".bold();

                        vec![
                            format!("try using the `{back}` command - the instruction that brought you here was:\n{inst_str}"),
                        ]
                    } else {
                        // how the hell did we get here?
                        // maybe we can analytic these cases in future...
                        vec![]
                    }
                } else {
                    // there's literally no instructions
                    vec![]
                }                
            }

            Error::Uninitialised { .. } => {
                vec![]
            }

            Error::UnalignedAccess { addr, alignment_requirement } => {
                let state = runtime.timeline().state();
                let inst = state.read_mem_word(state.pc()).unwrap();
                
                let decompiled = decompile::decompile_inst_into_parts(binary, inst_set, inst, state.pc());

                let half_aligned = *addr % 2 == 0;

                let equiv_instruction = match decompiled.inst_name.as_deref().unwrap() {
                    "lw" if half_aligned => Some("lh/lb"),
                    "sw" if half_aligned => Some("sh/sb"),
                    "lw"                 => Some("lb"),
                    "sw"                 => Some("sb"),
                    "lh"                 => Some("lb"),
                    "sh"                 => Some("sb"),
                    "lhu"                => Some("lbu"),
                    "shu"                => Some("sbu"),
                    _                    => None,
                };
                
                vec![
                    format!(
                        "you may have forgotten to multiply an index by {}{}",
                        match alignment_requirement {
                            AlignmentRequirement::Half => 2,
                            AlignmentRequirement::Word => 4,
                        },
                        match equiv_instruction {
                            Some(equiv_instruction) => format!(" (or use a `{}` instruction instead)", equiv_instruction.bold()),
                            None => String::new(),
                        },
                    )
                ]
            }

            Error::IntegerOverflow => {
                let mut tip = String::new();

                let state = runtime.timeline().state();
                let inst = state.read_mem_word(state.pc()).unwrap();
                let decompiled =
                    decompile::decompile_inst_into_parts(binary, inst_set, inst, state.pc());

                let adding = decompiled
                    .inst_name
                    .as_ref()
                    .map(|name| name.contains("add"))
                    .unwrap_or(false);

                let rs = (inst >> 21) & 0x1F;
                let rs_value = runtime.timeline().state().read_register(rs).unwrap();

                let value = if let Ok(imm) = decompiled.arguments[2].parse::<i16>() {
                    imm as i32
                } else {
                    let rt = (inst >> 16) & 0x1F;
                    let value = runtime.timeline().state().read_register(rt).unwrap();

                    value
                };

                let expected = if adding {
                    rs_value.wrapping_add(value)
                } else {
                    rs_value.wrapping_sub(value)
                };

                tip.push_str(&format!(
                    "if you expected the result to be {} (i.e. ignore overflow),\n",
                    expected
                ));

                if let Some(name) = match decompiled.inst_name.as_deref() {
                    Some("add") => Some("addu"),
                    Some("addi") => Some("addiu"),
                    Some("sub") => Some("subu"),
                    _ => unreachable!(),
                } {
                    tip.push_str(&format!(
                        "     then try using the equivalent unsigned instruction: `{}`\n",
                        name.bold()
                    ));
                }

                vec![tip]
            }

            Error::DivisionByZero => {
                vec![]
            }

            Error::SegmentationFault { addr, access: _ } => {
                let addr = *addr;

                match get_segment(addr) {
                    _ if addr == 0 => {
                        vec![format!("the address `{}{}` is {}\n", "0x".bold(), format!("{:08x}", addr).bold(), "NULL".yellow())]
                    }
                    Segment::None => {
                        vec![format!("the address `{}{}` is not part of any segment\n", "0x".bold(), format!("{:08x}", addr).bold())]
                    }
                    Segment::Text => {
                        vec![format!("the address `{}{}` is part of the {} segment\n", "0x".bold(), format!("{:08x}", addr).bold(), "TEXT".yellow())]
                    }
                    Segment::Data => {
                        vec![format!("the address `{}{}` is part of the {} segment\n", "0x".bold(), format!("{:08x}", addr).bold(), "DATA".yellow())]
                    }
                    Segment::Stack => {
                        vec![format!("the address `{}{}` is part of the {} segment\n", "0x".bold(), format!("{:08x}", addr).bold(), "STACK".yellow())]
                    }
                    Segment::KText => {
                        vec![format!("the address `{}{}` is part of the {} segment\n", "0x".bold(), format!("{:08x}", addr).bold(), "KTEXT".yellow())]
                    }
                    Segment::KData => {
                        vec![format!("the address `{}{}` is part of the {} segment\n", "0x".bold(), format!("{:08x}", addr).bold(), "KDATA".yellow())]
                    }
                }
            }

            Error::InvalidSyscall { .. } => {
                vec![]
            }
        }
    }
}

fn get_last_mod(runtime: &Runtime, write_marker: u32) -> Option<(usize, &State)> {
    println!();
    for i in (0..runtime.timeline().timeline_len()).rev() {
        let old_state = runtime.timeline().nth_state(i).unwrap();

        if old_state.write_marker() & (1u64 << write_marker) != 0 {
            return Some((i, old_state));
        }
    }

    None
}

fn get_real_instruction_start<'inst_set>(state: &State, binary: &Binary, inst_set: &'inst_set InstSet, pseudo_address: u32) -> Option<Decompiled<'inst_set>> {
    let mut real_inst_addr = pseudo_address;
    loop {
        let prev_inst = match state.read_mem_word(real_inst_addr) {
            Ok(real_inst) => real_inst,
            Err(_) => break None,
        };

        let real_inst_parts = decompile_inst_into_parts(binary, inst_set, prev_inst, real_inst_addr);

        if real_inst_parts.location.is_some() {
            break Some(real_inst_parts);
        }

        real_inst_addr -= 4;
    }
}

fn try_find_pseudo_expansion_end(program: &Binary, state: &State, initial_addr: u32) -> u32 {
    let mut addr = initial_addr + 4;

    loop {
        if program.line_numbers.get(&addr).is_some() {
            return addr;
        }

        if state.read_mem_word(addr).is_err() {
            return addr;
        }

        addr += 4;
    }
}
