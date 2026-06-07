use super::super::core;
use super::{MATCH_CONTEXT_WORDS, boxed_tag, heap_slice, jump_label, read_word, write_word};
use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::Literal;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::Process;
use crate::term::Term;
use crate::term::binary::{packed_word_count, write_binary};
use crate::term::binary_ref::BinaryRef;
use crate::term::boxed::{BoxedHeader, BoxedTag, ProcBin, write_float};
use crate::term::sub_binary::{SUB_BINARY_WORDS, write_sub_binary};
type GetOperands<'a> = (
    &'a Operand,
    &'a Operand,
    &'a Operand,
    &'a Operand,
    &'a Operand,
    &'a Operand,
);
pub(super) fn bs_start_match(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, source, destination) = match operands {
        [fail, source, destination] => (fail, source, destination),
        [fail, source, _live, destination] => (fail, source, destination),
        _ => return Err(ExecError::InvalidOperand("bs_start_match operands")),
    };
    let source = core::read_term(process, module, source)?;
    let Some(binary) = BinaryRef::new(source) else {
        return jump_label(module, fail);
    };
    let ptr = process
        .heap_mut()
        .alloc(MATCH_CONTEXT_WORDS)
        .map_err(ExecError::from)?;
    let heap = heap_slice(ptr, MATCH_CONTEXT_WORDS);
    heap[0] = BoxedHeader::new(BoxedTag::MatchContext, MATCH_CONTEXT_WORDS - 1);
    heap[1] = 0;
    heap[2] = (binary.len() * u8::BITS as usize) as u64;
    heap[3] = source.raw();
    core::write_term(process, destination, Term::boxed_ptr(heap.as_ptr()))?;
    Ok(InstructionOutcome::Continue)
}
pub(super) fn bs_get_integer(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, size, unit, flags, destination) =
        parse_get_operands(operands, "bs_get_integer2")?;
    let context = read_context(process, module, context)?;
    match get_integer_value(context, size, unit, flags)? {
        Some((value, bits)) => {
            let term = Term::try_small_int(value).ok_or(ExecError::Badarg)?;
            core::write_term(process, destination, term)?;
            context.set_position_bits(context.position_bits() + bits);
            Ok(InstructionOutcome::Continue)
        }
        None => jump_label(module, fail),
    }
}
pub(super) fn bs_get_float(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, size, unit, flags, destination) =
        parse_get_operands(operands, "bs_get_float2")?;
    let context = read_context(process, module, context)?;
    match get_float_value(context, size, unit, flags)? {
        Some((value, bits)) => {
            if process.heap().available() < 2 {
                return Err(ExecError::GcNeeded {
                    requested: 2,
                    available: process.heap().available(),
                });
            }
            let ptr = process.heap_mut().alloc(2).map_err(ExecError::from)?;
            let term = write_float(heap_slice(ptr, 2), value).ok_or(ExecError::Badarg)?;
            core::write_term(process, destination, term)?;
            context.set_position_bits(context.position_bits() + bits);
            Ok(InstructionOutcome::Continue)
        }
        None => jump_label(module, fail),
    }
}
pub(super) fn bs_get_binary(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, size, unit, flags, destination) =
        parse_get_operands(operands, "bs_get_binary2")?;
    let context = read_context(process, module, context)?;
    match get_binary_bytes(context, size, unit, flags)? {
        Some((bytes, bits)) => {
            let binary = allocate_extracted_binary(process, context, bytes, bits)?;
            core::write_term(process, destination, binary)?;
            context.set_position_bits(context.position_bits() + bits);
            Ok(InstructionOutcome::Continue)
        }
        None => jump_label(module, fail),
    }
}
pub(super) fn bs_skip_bits(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, size, unit) = match operands {
        [fail, context, size, unit, _flags] => (fail, context, size, unit),
        _ => return Err(ExecError::InvalidOperand("bs_skip_bits2 operands")),
    };
    let bits = segment_bits(size, unit)?;
    let context = read_context(process, module, context)?;
    if !context.has_bits(bits) {
        return jump_label(module, fail);
    }
    context.set_position_bits(context.position_bits() + bits);
    Ok(InstructionOutcome::Continue)
}
pub(super) fn bs_match_string(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, bit_len, literal) = match operands {
        [fail, context, bit_len, literal] => (fail, context, bit_len, literal),
        _ => return Err(ExecError::InvalidOperand("bs_match_string operands")),
    };
    let bit_len = core::operand_usize(bit_len, "bs_match_string bit length")?;
    let expected = literal_bytes(module, literal, bit_len / u8::BITS as usize)?;
    let context = read_context(process, module, context)?;
    if match_bytes(context, bit_len, expected)? {
        context.set_position_bits(context.position_bits() + bit_len);
        Ok(InstructionOutcome::Continue)
    } else {
        jump_label(module, fail)
    }
}
pub(super) fn bs_test_tail(
    process: &Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, expected) = match operands {
        [fail, context, expected] => (fail, context, expected),
        _ => return Err(ExecError::InvalidOperand("bs_test_tail2 operands")),
    };
    let expected = core::operand_usize(expected, "bs_test_tail2 remaining bits")?;
    let context = read_context(process, module, context)?;
    if context.remaining_bits() == expected {
        Ok(InstructionOutcome::Continue)
    } else {
        jump_label(module, fail)
    }
}
pub(super) fn bs_test_unit(
    process: &Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, unit) = match operands {
        [fail, context, unit] => (fail, context, unit),
        _ => return Err(ExecError::InvalidOperand("bs_test_unit operands")),
    };
    let unit = core::operand_usize(unit, "bs_test_unit unit")?;
    if unit == 0 {
        return Err(ExecError::Badarg);
    }
    let context = read_context(process, module, context)?;
    if context.remaining_bits().is_multiple_of(unit) {
        Ok(InstructionOutcome::Continue)
    } else {
        jump_label(module, fail)
    }
}
pub(super) fn bs_get_tail(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (context, destination) = match operands {
        [_fail, context, _live, destination] => (context, destination),
        [_fail, context, destination] => (context, destination),
        _ => return Err(ExecError::InvalidOperand("bs_get_tail operands")),
    };
    let context = read_context(process, module, context)?;
    if !context.position_bits().is_multiple_of(u8::BITS as usize) {
        return Err(ExecError::Badarg);
    }
    let bits = context.remaining_bits();
    if !bits.is_multiple_of(u8::BITS as usize) {
        return Err(ExecError::Badarg);
    }
    let bytes = context.slice(bits).ok_or(ExecError::Badarg)?;
    let binary = allocate_binary(process, bytes)?;
    core::write_term(process, destination, binary)?;
    context.set_position_bits(context.total_bits());
    Ok(InstructionOutcome::Continue)
}
pub(super) fn bs_get_position(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (context, destination) = match operands {
        [context, destination, _live] => (context, destination),
        _ => return Err(ExecError::InvalidOperand("bs_get_position operands")),
    };
    let context = read_context(process, module, context)?;
    let term = Term::try_small_int(context.position_bits() as i64).ok_or(ExecError::Badarg)?;
    core::write_term(process, destination, term)?;
    Ok(InstructionOutcome::Continue)
}
pub(super) fn bs_set_position(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (context, source) = match operands {
        [context, source] => (context, source),
        _ => return Err(ExecError::InvalidOperand("bs_set_position operands")),
    };
    let context = read_context(process, module, context)?;
    let position = core::read_term(process, module, source)?
        .as_small_int()
        .ok_or(ExecError::Badarg)?;
    if position < 0 || position as usize > context.total_bits() {
        return Err(ExecError::Badarg);
    }
    context.set_position_bits(position as usize);
    Ok(InstructionOutcome::Continue)
}
pub(super) fn bs_match(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context_operand, commands) = match operands {
        [fail, context, Operand::List(commands)] => (fail, context, commands.as_slice()),
        [fail, context, rest @ ..] => (fail, context, rest),
        _ => return Err(ExecError::InvalidOperand("bs_match operands")),
    };
    let context = read_context(process, module, context_operand)?;
    let saved = context.position_bits();
    let result = run_match_commands(process, module, context, commands);
    match result {
        Ok(true) => Ok(InstructionOutcome::Continue),
        Ok(false) => {
            context.set_position_bits(saved);
            jump_label(module, fail)
        }
        Err(error) => {
            context.set_position_bits(saved);
            Err(error)
        }
    }
}
fn run_match_commands(
    process: &mut Process,
    module: &Module,
    context: MatchContext,
    commands: &[Operand],
) -> Result<bool, ExecError> {
    if commands
        .iter()
        .all(|command| matches!(command, Operand::List(_)))
    {
        for command in commands {
            if !run_one_nested_command(process, module, context, command)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    let mut index = 0;
    while index < commands.len() {
        let tag = command_name(&commands[index])?;
        index += 1;
        if !run_flat_command(process, module, context, tag, commands, &mut index)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn run_one_nested_command(
    process: &mut Process,
    module: &Module,
    context: MatchContext,
    command: &Operand,
) -> Result<bool, ExecError> {
    let Operand::List(items) = command else {
        return Err(ExecError::InvalidOperand("bs_match command"));
    };
    let Some((tag, args)) = items.split_first() else {
        return Err(ExecError::InvalidOperand("bs_match command"));
    };
    run_command_args(process, module, context, command_name(tag)?, args)
}
fn run_flat_command(
    process: &mut Process,
    module: &Module,
    context: MatchContext,
    tag: &str,
    commands: &[Operand],
    index: &mut usize,
) -> Result<bool, ExecError> {
    let arity = match tag {
        "ensure" | "ensure_at_least" | "ensure_exactly" => 2,
        "integer" | "float" | "binary" => 5,
        "skip" => 1,
        "get_tail" => {
            if commands.len().saturating_sub(*index) >= 3 {
                3
            } else {
                2
            }
        }
        "=:=" => 3,
        _ => return Err(ExecError::InvalidOperand("bs_match command")),
    };
    let end = index
        .checked_add(arity)
        .ok_or(ExecError::InvalidOperand("bs_match command"))?;
    let args = commands
        .get(*index..end)
        .ok_or(ExecError::InvalidOperand("bs_match command"))?;
    *index = end;
    run_command_args(process, module, context, tag, args)
}
fn run_command_args(
    process: &mut Process,
    module: &Module,
    context: MatchContext,
    tag: &str,
    args: &[Operand],
) -> Result<bool, ExecError> {
    match (tag, args) {
        ("ensure" | "ensure_at_least", [_live, bits]) => {
            Ok(context.has_bits(core::operand_usize(bits, "bs_match ensure bits")?))
        }
        ("ensure_exactly", [stride, _unit]) => {
            Ok(context.remaining_bits() == core::operand_usize(stride, "bs_match ensure exactly")?)
        }
        ("integer", [_live, flags, size, unit, dst]) => {
            let Some((value, bits)) = get_integer_value(context, size, unit, flags)? else {
                return Ok(false);
            };
            let term = Term::try_small_int(value).ok_or(ExecError::Badarg)?;
            core::write_term(process, dst, term)?;
            context.set_position_bits(context.position_bits() + bits);
            Ok(true)
        }
        ("float", [_live, flags, size, unit, dst]) => {
            let Some((value, bits)) = get_float_value(context, size, unit, flags)? else {
                return Ok(false);
            };
            if process.heap().available() < 2 {
                return Err(ExecError::GcNeeded {
                    requested: 2,
                    available: process.heap().available(),
                });
            }
            let ptr = process.heap_mut().alloc(2).map_err(ExecError::from)?;
            let term = write_float(heap_slice(ptr, 2), value).ok_or(ExecError::Badarg)?;
            core::write_term(process, dst, term)?;
            context.set_position_bits(context.position_bits() + bits);
            Ok(true)
        }
        ("binary", [_live, flags, size, unit, dst]) => {
            let Some((bytes, bits)) = get_binary_bytes(context, size, unit, flags)? else {
                return Ok(false);
            };
            let binary = allocate_extracted_binary(process, context, bytes, bits)?;
            core::write_term(process, dst, binary)?;
            context.set_position_bits(context.position_bits() + bits);
            Ok(true)
        }
        ("skip", [stride]) => {
            let bits = core::operand_usize(stride, "bs_match skip stride")?;
            if !context.has_bits(bits) {
                return Ok(false);
            }
            context.set_position_bits(context.position_bits() + bits);
            Ok(true)
        }
        ("get_tail", [_live, dst]) | ("get_tail", [_live, _, dst]) => {
            if !context.position_bits().is_multiple_of(u8::BITS as usize) {
                return Ok(false);
            }
            let bits = context.remaining_bits();
            let bytes = context.slice(bits).ok_or(ExecError::Badarg)?;
            let binary = allocate_binary(process, bytes)?;
            core::write_term(process, dst, binary)?;
            context.set_position_bits(context.total_bits());
            Ok(true)
        }
        ("=:=", [_live, bits, value]) => {
            let bits = core::operand_usize(bits, "bs_match exact bits")?;
            let expected = exact_value_bytes(module, value, bits)?;
            if match_bytes(context, bits, expected)? {
                context.set_position_bits(context.position_bits() + bits);
                Ok(true)
            } else {
                Ok(false)
            }
        }
        _ => Err(ExecError::InvalidOperand("bs_match command")),
    }
}
fn get_integer_value(
    context: MatchContext,
    size: &Operand,
    unit: &Operand,
    flags: &Operand,
) -> Result<Option<(i64, usize)>, ExecError> {
    let size_bits = segment_bits(size, unit)?;
    let flags = SegmentFlags::from_flags(flags);
    if !size_bits.is_multiple_of(u8::BITS as usize)
        || !context.position_bits().is_multiple_of(u8::BITS as usize)
    {
        return Err(ExecError::Badarg);
    }
    if !context.has_bits(size_bits) {
        return Ok(None);
    }
    let bytes = context.slice(size_bits).ok_or(ExecError::Badarg)?;
    Ok(Some((decode_integer(bytes, flags)?, size_bits)))
}
fn get_float_value(
    context: MatchContext,
    size: &Operand,
    unit: &Operand,
    flags: &Operand,
) -> Result<Option<(f64, usize)>, ExecError> {
    let bits = segment_bits(size, unit)?;
    if !matches!(bits, 32 | 64) || !context.position_bits().is_multiple_of(u8::BITS as usize) {
        return Err(ExecError::Badarg);
    }
    if !context.has_bits(bits) {
        return Ok(None);
    }
    let bytes = context.slice(bits).ok_or(ExecError::Badarg)?;
    let value = match (bits, Endian::from_flags(flags)) {
        (32, Endian::Big) => {
            f32::from_bits(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])) as f64
        }
        (32, Endian::Little) => {
            f32::from_bits(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])) as f64
        }
        (64, Endian::Big) => f64::from_bits(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        (64, Endian::Little) => f64::from_bits(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ])),
        _ => return Err(ExecError::Badarg),
    };
    Ok(Some((value, bits)))
}
fn get_binary_bytes(
    context: MatchContext,
    size: &Operand,
    unit: &Operand,
    _flags: &Operand,
) -> Result<Option<(&'static [u8], usize)>, ExecError> {
    let bits = segment_bits(size, unit)?;
    if !bits.is_multiple_of(u8::BITS as usize)
        || !context.position_bits().is_multiple_of(u8::BITS as usize)
    {
        return Err(ExecError::Badarg);
    }
    if !context.has_bits(bits) {
        return Ok(None);
    }
    Ok(Some((context.slice(bits).ok_or(ExecError::Badarg)?, bits)))
}
fn match_bytes(context: MatchContext, bits: usize, expected: &[u8]) -> Result<bool, ExecError> {
    if !bits.is_multiple_of(u8::BITS as usize) {
        return Err(ExecError::Badarg);
    }
    if !context.position_bits().is_multiple_of(u8::BITS as usize) || !context.has_bits(bits) {
        return Ok(false);
    }
    Ok(context.slice(bits).ok_or(ExecError::Badarg)? == expected)
}
fn allocate_binary(process: &mut Process, bytes: &[u8]) -> Result<Term, ExecError> {
    let words = 2 + packed_word_count(bytes.len());
    if process.heap().available() < words {
        return Err(ExecError::GcNeeded {
            requested: words,
            available: process.heap().available(),
        });
    }
    let ptr = process.heap_mut().alloc(words).map_err(ExecError::from)?;
    write_binary(heap_slice(ptr, words), bytes).ok_or(ExecError::Badarg)
}
fn allocate_extracted_binary(
    process: &mut Process,
    context: MatchContext,
    bytes: &[u8],
    bits: usize,
) -> Result<Term, ExecError> {
    let source = context.source_term();
    if ProcBin::new(source).is_some() {
        let start = context.position_bits() / u8::BITS as usize;
        let length = bits / u8::BITS as usize;
        if process.heap().available() < SUB_BINARY_WORDS {
            return Err(ExecError::GcNeeded {
                requested: SUB_BINARY_WORDS,
                available: process.heap().available(),
            });
        }
        let ptr = process
            .heap_mut()
            .alloc(SUB_BINARY_WORDS)
            .map_err(ExecError::from)?;
        return write_sub_binary(heap_slice(ptr, SUB_BINARY_WORDS), source, start, length)
            .ok_or(ExecError::Badarg);
    }

    allocate_binary(process, bytes)
}
fn read_context(
    process: &Process,
    module: &Module,
    operand: &Operand,
) -> Result<MatchContext, ExecError> {
    MatchContext::new(core::read_term(process, module, operand)?).ok_or(ExecError::Badarg)
}
fn command_name(operand: &Operand) -> Result<&'static str, ExecError> {
    match operand {
        Operand::Atom(None) => Ok("=:="),
        Operand::Unsigned(0) | Operand::Integer(0) => Ok("ensure_at_least"),
        Operand::Unsigned(1) | Operand::Integer(1) => Ok("ensure_exactly"),
        Operand::Unsigned(2) | Operand::Integer(2) => Ok("integer"),
        Operand::Unsigned(3) | Operand::Integer(3) => Ok("float"),
        Operand::Unsigned(4) | Operand::Integer(4) => Ok("binary"),
        Operand::Unsigned(5) | Operand::Integer(5) => Ok("skip"),
        Operand::Unsigned(6) | Operand::Integer(6) => Ok("get_tail"),
        _ => Err(ExecError::InvalidOperand("bs_match command")),
    }
}
pub(super) fn segment_bits(size: &Operand, unit: &Operand) -> Result<usize, ExecError> {
    let size = core::operand_usize(size, "segment size")?;
    let unit = core::operand_usize(unit, "segment unit")?;
    size.checked_mul(unit)
        .ok_or(ExecError::InvalidOperand("segment size"))
}
fn literal_bytes<'a>(
    module: &'a Module,
    operand: &'a Operand,
    byte_len: usize,
) -> Result<&'a [u8], ExecError> {
    match operand {
        Operand::Literal(index) => match module.literals.get(*index) {
            Some(Literal::Binary(bytes) | Literal::String(bytes)) => bytes
                .get(..byte_len)
                .filter(|bytes| bytes.len() == byte_len)
                .ok_or(ExecError::Badarg),
            _ => Err(ExecError::Badarg),
        },
        offset => {
            let offset = core::operand_usize(offset, "string table offset")?;
            module
                .string_table
                .get(offset..offset + byte_len)
                .ok_or(ExecError::Badarg)
        }
    }
}
fn exact_value_bytes<'a>(
    module: &'a Module,
    operand: &'a Operand,
    bits: usize,
) -> Result<&'a [u8], ExecError> {
    if !bits.is_multiple_of(u8::BITS as usize) {
        return Err(ExecError::Badarg);
    }
    literal_bytes(module, operand, bits / u8::BITS as usize)
}
#[derive(Copy, Clone)]
pub(crate) struct MatchContext {
    ptr: *mut u64,
}
impl MatchContext {
    pub(crate) fn new(term: Term) -> Option<Self> {
        let ptr = term.heap_ptr()? as *mut u64;
        (boxed_tag(ptr) == Some(BoxedTag::MatchContext)).then_some(Self { ptr })
    }
    pub(crate) fn position_bits(self) -> usize {
        read_word(self.ptr, 1) as usize
    }
    pub(crate) fn set_position_bits(self, bits: usize) {
        write_word(self.ptr, 1, bits as u64);
    }
    pub(crate) fn total_bits(self) -> usize {
        read_word(self.ptr, 2) as usize
    }
    fn source_term(self) -> Term {
        Term::from_raw(read_word(self.ptr, 3))
    }
    fn source(self) -> Option<BinaryRef> {
        BinaryRef::new(self.source_term())
    }
    pub(crate) fn remaining_bits(self) -> usize {
        self.total_bits().saturating_sub(self.position_bits())
    }
    pub(crate) fn has_bits(self, bits: usize) -> bool {
        self.position_bits()
            .checked_add(bits)
            .is_some_and(|end| end <= self.total_bits())
    }
    pub(crate) fn slice(self, bits: usize) -> Option<&'static [u8]> {
        if !bits.is_multiple_of(u8::BITS as usize)
            || !self.position_bits().is_multiple_of(u8::BITS as usize)
        {
            return None;
        }
        let start = self.position_bits() / u8::BITS as usize;
        let len = bits / u8::BITS as usize;
        let end = start.checked_add(len)?;
        self.source()?.as_bytes().get(start..end)
    }
}
#[derive(Copy, Clone)]
pub(crate) enum Endian {
    Big,
    Little,
}
impl Endian {
    pub(crate) fn from_flags(flags: &Operand) -> Self {
        match flags {
            Operand::Unsigned(1) | Operand::Integer(1) => Self::Little,
            Operand::List(items) if items.iter().any(is_little_flag) => Self::Little,
            Operand::Unsigned(v) if v & 0x02 != 0 => Self::Little,
            Operand::Integer(v) if v & 0x02 != 0 => Self::Little,
            _ => Self::Big,
        }
    }
}
#[derive(Copy, Clone)]
pub(crate) struct SegmentFlags {
    pub(crate) endian: Endian,
    pub(crate) signed: bool,
}
impl SegmentFlags {
    fn from_flags(flags: &Operand) -> Self {
        let signed = match flags {
            Operand::Unsigned(v) => v & 0x04 != 0,
            Operand::Integer(v) => v & 0x04 != 0,
            Operand::List(items) => items.iter().any(is_signed_flag),
            _ => false,
        };
        Self {
            endian: Endian::from_flags(flags),
            signed,
        }
    }
}
fn is_signed_flag(flag: &Operand) -> bool {
    matches!(flag, Operand::Unsigned(v) if v & 0x04 != 0)
        || matches!(flag, Operand::Integer(v) if v & 0x04 != 0)
}
fn is_little_flag(flag: &Operand) -> bool {
    matches!(flag, Operand::Unsigned(1) | Operand::Integer(1))
}
fn parse_get_operands<'a>(
    operands: &'a [Operand],
    context: &'static str,
) -> Result<GetOperands<'a>, ExecError> {
    match operands {
        [fail, match_context, _live, size, unit, flags, destination] => {
            Ok((fail, match_context, size, unit, flags, destination))
        }
        [fail, match_context, size, unit, flags, destination] => {
            Ok((fail, match_context, size, unit, flags, destination))
        }
        _ => Err(ExecError::InvalidOperand(context)),
    }
}
pub(crate) fn decode_integer(bytes: &[u8], flags: SegmentFlags) -> Result<i64, ExecError> {
    if bytes.len() > std::mem::size_of::<i64>() {
        return Err(ExecError::Badarg);
    }
    let msb = match flags.endian {
        Endian::Big => bytes.first(),
        Endian::Little => bytes.last(),
    };
    let negative = flags.signed && msb.is_some_and(|byte| byte & 0x80 != 0);
    let fill = if negative { 0xff_u8 } else { 0x00_u8 };
    let mut full = [fill; 8];
    match flags.endian {
        Endian::Big => full[8 - bytes.len()..].copy_from_slice(bytes),
        Endian::Little => full[..bytes.len()].copy_from_slice(bytes),
    }
    Ok(match flags.endian {
        Endian::Big => u64::from_be_bytes(full) as i64,
        Endian::Little => u64::from_le_bytes(full) as i64,
    })
}
