use crate::error::ExecError;
use crate::interpreter::InstructionOutcome;
use crate::loader::decode::compact::Operand;
use crate::module::Module;
use crate::process::Process;
use crate::term::Term;

use super::super::core;
use super::jump_label;
use super::matching::{Endian, MatchContext};

type UtfDecoder = fn(MatchContext, Endian) -> Result<Option<(u32, usize)>, ExecError>;

pub(super) fn bs_get_utf8(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    utf_get(
        process,
        module,
        operands,
        "bs_get_utf8 operands",
        decode_utf8,
    )
}

pub(super) fn bs_skip_utf8(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    utf_skip(
        process,
        module,
        operands,
        "bs_skip_utf8 operands",
        decode_utf8,
    )
}

pub(super) fn bs_get_utf16(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, flags, destination) =
        parse_get_operands(operands, "bs_get_utf16 operands")?;
    utf_get_with_endian(
        process,
        module,
        fail,
        context,
        flags,
        destination,
        decode_utf16,
    )
}

pub(super) fn bs_skip_utf16(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, flags) = parse_skip_operands(operands, "bs_skip_utf16 operands")?;
    utf_skip_with_endian(process, module, fail, context, flags, decode_utf16)
}

pub(super) fn bs_get_utf32(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, flags, destination) =
        parse_get_operands(operands, "bs_get_utf32 operands")?;
    utf_get_with_endian(
        process,
        module,
        fail,
        context,
        flags,
        destination,
        decode_utf32,
    )
}

pub(super) fn bs_skip_utf32(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, flags) = parse_skip_operands(operands, "bs_skip_utf32 operands")?;
    utf_skip_with_endian(process, module, fail, context, flags, decode_utf32)
}

fn utf_get(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
    invalid: &'static str,
    decoder: UtfDecoder,
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, flags, destination) = parse_get_operands(operands, invalid)?;
    utf_get_with_endian(process, module, fail, context, flags, destination, decoder)
}

fn utf_skip(
    process: &mut Process,
    module: &Module,
    operands: &[Operand],
    invalid: &'static str,
    decoder: UtfDecoder,
) -> Result<InstructionOutcome, ExecError> {
    let (fail, context, flags) = parse_skip_operands(operands, invalid)?;
    utf_skip_with_endian(process, module, fail, context, flags, decoder)
}

fn utf_get_with_endian(
    process: &mut Process,
    module: &Module,
    fail: &Operand,
    context: &Operand,
    flags: &Operand,
    destination: &Operand,
    decoder: UtfDecoder,
) -> Result<InstructionOutcome, ExecError> {
    let context = read_context(process, module, context)?;
    match decoder(context, Endian::from_flags(flags))? {
        Some((codepoint, bits)) => {
            let term = Term::try_small_int(i64::from(codepoint)).ok_or(ExecError::Badarg)?;
            core::write_term(process, destination, term)?;
            context.set_position_bits(context.position_bits() + bits);
            Ok(InstructionOutcome::Continue)
        }
        None => jump_label(module, fail),
    }
}

fn utf_skip_with_endian(
    process: &mut Process,
    module: &Module,
    fail: &Operand,
    context: &Operand,
    flags: &Operand,
    decoder: UtfDecoder,
) -> Result<InstructionOutcome, ExecError> {
    let context = read_context(process, module, context)?;
    match decoder(context, Endian::from_flags(flags))? {
        Some((_codepoint, bits)) => {
            context.set_position_bits(context.position_bits() + bits);
            Ok(InstructionOutcome::Continue)
        }
        None => jump_label(module, fail),
    }
}

fn parse_get_operands<'a>(
    operands: &'a [Operand],
    invalid: &'static str,
) -> Result<(&'a Operand, &'a Operand, &'a Operand, &'a Operand), ExecError> {
    match operands {
        [fail, context, _live, flags, destination] => Ok((fail, context, flags, destination)),
        [fail, context, flags, destination] => Ok((fail, context, flags, destination)),
        _ => Err(ExecError::InvalidOperand(invalid)),
    }
}

fn parse_skip_operands<'a>(
    operands: &'a [Operand],
    invalid: &'static str,
) -> Result<(&'a Operand, &'a Operand, &'a Operand), ExecError> {
    match operands {
        [fail, context, _live, flags] => Ok((fail, context, flags)),
        [fail, context, flags] => Ok((fail, context, flags)),
        _ => Err(ExecError::InvalidOperand(invalid)),
    }
}

fn read_context(
    process: &Process,
    module: &Module,
    operand: &Operand,
) -> Result<MatchContext, ExecError> {
    MatchContext::new(core::read_term(process, module, operand)?).ok_or(ExecError::Badarg)
}

fn decode_utf8(context: MatchContext, _endian: Endian) -> Result<Option<(u32, usize)>, ExecError> {
    if !context.position_bits().is_multiple_of(u8::BITS as usize) {
        return Ok(None);
    }
    let bytes = context
        .slice(context.remaining_bits())
        .ok_or(ExecError::Badarg)?;
    let Some(first) = bytes.first().copied() else {
        return Ok(None);
    };
    let (needed, mut codepoint, min) = if first <= 0x7f {
        (1, u32::from(first), 0)
    } else if (0xc2..=0xdf).contains(&first) {
        (2, u32::from(first & 0x1f), 0x80)
    } else if (0xe0..=0xef).contains(&first) {
        (3, u32::from(first & 0x0f), 0x800)
    } else if (0xf0..=0xf4).contains(&first) {
        (4, u32::from(first & 0x07), 0x10000)
    } else {
        return Ok(None);
    };
    if bytes.len() < needed {
        return Ok(None);
    }
    for byte in &bytes[1..needed] {
        if byte & 0xc0 != 0x80 {
            return Ok(None);
        }
        codepoint = (codepoint << 6) | u32::from(byte & 0x3f);
    }
    if codepoint < min || !valid_codepoint(codepoint) {
        return Ok(None);
    }
    Ok(Some((codepoint, needed * u8::BITS as usize)))
}

fn decode_utf16(context: MatchContext, endian: Endian) -> Result<Option<(u32, usize)>, ExecError> {
    let Some(first) = read_u16(context, 0, endian)? else {
        return Ok(None);
    };
    if (0xd800..=0xdbff).contains(&first) {
        let Some(second) = read_u16(context, 2, endian)? else {
            return Ok(None);
        };
        if !(0xdc00..=0xdfff).contains(&second) {
            return Ok(None);
        }
        let high = u32::from(first) - 0xd800;
        let low = u32::from(second) - 0xdc00;
        let codepoint = 0x10000 + ((high << 10) | low);
        if valid_codepoint(codepoint) {
            Ok(Some((codepoint, 32)))
        } else {
            Ok(None)
        }
    } else if (0xdc00..=0xdfff).contains(&first) {
        Ok(None)
    } else {
        Ok(Some((u32::from(first), 16)))
    }
}

fn decode_utf32(context: MatchContext, endian: Endian) -> Result<Option<(u32, usize)>, ExecError> {
    if !context.position_bits().is_multiple_of(u8::BITS as usize) || !context.has_bits(32) {
        return Ok(None);
    }
    let bytes = context.slice(32).ok_or(ExecError::Badarg)?;
    let codepoint = match endian {
        Endian::Big => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        Endian::Little => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
    };
    if valid_codepoint(codepoint) {
        Ok(Some((codepoint, 32)))
    } else {
        Ok(None)
    }
}

fn read_u16(
    context: MatchContext,
    byte_offset: usize,
    endian: Endian,
) -> Result<Option<u16>, ExecError> {
    if !context.position_bits().is_multiple_of(u8::BITS as usize) {
        return Ok(None);
    }
    let bits = (byte_offset + 2) * u8::BITS as usize;
    if !context.has_bits(bits) {
        return Ok(None);
    }
    let bytes = context.slice(bits).ok_or(ExecError::Badarg)?;
    let pair = [bytes[byte_offset], bytes[byte_offset + 1]];
    Ok(Some(match endian {
        Endian::Big => u16::from_be_bytes(pair),
        Endian::Little => u16::from_le_bytes(pair),
    }))
}

fn valid_codepoint(codepoint: u32) -> bool {
    codepoint <= 0x10ffff && !(0xd800..=0xdfff).contains(&codepoint)
}
