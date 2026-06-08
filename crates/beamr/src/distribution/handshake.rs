//! OTP 23+ BEAM distribution handshake support.
//!
//! Handshake packets use the distribution setup framing: a 16-bit big-endian
//! packet length followed by a tagged payload. After this handshake succeeds,
//! distribution traffic switches to the normal four-byte distribution header.

use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};

const TAG_NEW_NAME: u8 = b'N';
const TAG_STATUS: u8 = b's';
const TAG_CHALLENGE_REPLY: u8 = b'r';
const TAG_CHALLENGE_ACK: u8 = b'a';
const DIGEST_LEN: usize = 16;

/// BEAM distribution capability flags exchanged during the OTP 23+ handshake.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct DistributionFlags(u64);

impl DistributionFlags {
    /// Node is published in EPMD.
    pub const PUBLISHED: Self = Self(0x1);
    /// Node supports atom cache references.
    pub const ATOM_CACHE: Self = Self(0x2);
    /// Node supports extended reference identifiers.
    pub const EXTENDED_REFERENCES: Self = Self(0x4);
    /// OTP wire name: `DFLAG_EXTENDED_PIDS_PORTS`.
    pub const EXTENDED_PIDS: Self = Self(0x100);
    /// Node supports UTF-8 atom encoding.
    pub const UTF8_ATOMS: Self = Self(0x10000);
    /// Node supports map tags.
    pub const MAP_TAG: Self = Self(0x20000);
    /// Node supports 32-bit creation values.
    pub const BIG_CREATION: Self = Self(0x40000);
    /// Node speaks the OTP 23+ version-6 handshake with 64-bit flags.
    pub const HANDSHAKE_23: Self = Self(0x1000000);
    /// No distribution flags.
    pub const EMPTY: Self = Self(0);

    /// Returns the default capability set offered by beamr for this handshake.
    pub const fn offered() -> Self {
        Self(
            Self::PUBLISHED.0
                | Self::ATOM_CACHE.0
                | Self::EXTENDED_REFERENCES.0
                | Self::EXTENDED_PIDS.0
                | Self::UTF8_ATOMS.0
                | Self::MAP_TAG.0
                | Self::BIG_CREATION.0
                | Self::HANDSHAKE_23.0,
        )
    }

    /// Returns the minimum capability set this implementation requires.
    pub const fn required() -> Self {
        Self(Self::HANDSHAKE_23.0)
    }

    /// Builds flags from their raw wire representation.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns the raw wire representation.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Returns `true` when all flags in `other` are present in `self`.
    pub const fn contains_all(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns the intersection of two flag sets.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Negotiates connection capabilities as the intersection of local and remote flags.
    pub fn negotiate(local: Self, remote: Self) -> Result<Self, HandshakeError> {
        let negotiated = local.intersection(remote);
        let required = Self::required();
        if negotiated.contains_all(required) {
            Ok(negotiated)
        } else {
            Err(HandshakeError::IncompatibleFlags {
                local,
                remote,
                required,
                negotiated,
            })
        }
    }
}

impl std::ops::BitOr for DistributionFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for DistributionFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

/// Local node metadata sent in handshake name and challenge packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeNode {
    name: String,
    creation: u32,
    flags: DistributionFlags,
}

impl HandshakeNode {
    /// Creates a node descriptor with explicit flags.
    pub fn new(
        name: impl Into<String>,
        creation: u32,
        flags: DistributionFlags,
    ) -> Result<Self, HandshakeError> {
        let name = name.into();
        validate_name(&name)?;
        Ok(Self {
            name,
            creation,
            flags,
        })
    }

    /// Creates a node descriptor using beamr's default offered flags.
    pub fn with_default_flags(
        name: impl Into<String>,
        creation: u32,
    ) -> Result<Self, HandshakeError> {
        Self::new(name, creation, DistributionFlags::offered())
    }

    /// Returns the node name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the node creation value.
    pub const fn creation(&self) -> u32 {
        self.creation
    }

    /// Returns the flags this node offers.
    pub const fn flags(&self) -> DistributionFlags {
        self.flags
    }
}

/// Successful handshake result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandshakeResult {
    remote_name: String,
    remote_creation: u32,
    remote_flags: DistributionFlags,
    negotiated_flags: DistributionFlags,
}

impl HandshakeResult {
    /// Creates a successful handshake result value.
    pub fn new(
        remote_name: impl Into<String>,
        remote_creation: u32,
        remote_flags: DistributionFlags,
        negotiated_flags: DistributionFlags,
    ) -> Result<Self, HandshakeError> {
        let remote_name = remote_name.into();
        validate_name(&remote_name)?;
        Ok(Self {
            remote_name,
            remote_creation,
            remote_flags,
            negotiated_flags,
        })
    }

    /// Returns the authenticated remote node name.
    pub fn remote_name(&self) -> &str {
        &self.remote_name
    }

    /// Returns the remote creation value.
    pub const fn remote_creation(&self) -> u32 {
        self.remote_creation
    }

    /// Returns the remote node's offered flags.
    pub const fn remote_flags(&self) -> DistributionFlags {
        self.remote_flags
    }

    /// Returns the negotiated distribution capabilities.
    pub const fn negotiated_flags(&self) -> DistributionFlags {
        self.negotiated_flags
    }
}

/// Distribution handshake failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeError {
    /// Underlying stream I/O failed.
    Io(String),
    /// A packet was structurally invalid.
    MalformedPacket(String),
    /// A packet carried the wrong tag for the current handshake step.
    UnexpectedTag { expected: u8, actual: u8 },
    /// The responder returned a non-success status.
    BadStatus(String),
    /// The peer does not share the minimum required distribution flags.
    IncompatibleFlags {
        local: DistributionFlags,
        remote: DistributionFlags,
        required: DistributionFlags,
        negotiated: DistributionFlags,
    },
    /// The MD5 challenge response or challenge ack did not match the cookie.
    DigestMismatch,
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "distribution handshake I/O failed: {message}"),
            Self::MalformedPacket(message) => {
                write!(
                    formatter,
                    "malformed distribution handshake packet: {message}"
                )
            }
            Self::UnexpectedTag { expected, actual } => write!(
                formatter,
                "unexpected distribution handshake tag: expected 0x{expected:02x}, got 0x{actual:02x}"
            ),
            Self::BadStatus(status) => {
                write!(
                    formatter,
                    "distribution handshake rejected with status {status:?}"
                )
            }
            Self::IncompatibleFlags {
                local,
                remote,
                required,
                negotiated,
            } => write!(
                formatter,
                "incompatible distribution flags: local=0x{:x}, remote=0x{:x}, required=0x{:x}, negotiated=0x{:x}",
                local.bits(),
                remote.bits(),
                required.bits(),
                negotiated.bits()
            ),
            Self::DigestMismatch => formatter.write_str("distribution handshake digest mismatch"),
        }
    }
}

impl Error for HandshakeError {}

impl From<io::Error> for HandshakeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// Runs the outbound OTP 23+ distribution handshake over an already-connected stream.
pub fn initiate_handshake<S: Read + Write>(
    stream: &mut S,
    local: &HandshakeNode,
    cookie: &str,
    challenge: u32,
) -> Result<HandshakeResult, HandshakeError> {
    write_packet(stream, &encode_name(local, None)?)?;

    let status = read_status_packet(stream)?;
    if !is_success_status(&status) {
        return Err(HandshakeError::BadStatus(status));
    }

    let remote = read_name_packet(stream, true)?;
    let negotiated_flags = DistributionFlags::negotiate(local.flags, remote.flags)?;

    let remote_digest = challenge_digest(
        cookie,
        remote.challenge.ok_or_else(|| {
            HandshakeError::MalformedPacket("challenge packet omitted challenge value".into())
        })?,
    );
    write_packet(stream, &encode_challenge_reply(challenge, remote_digest))?;

    let ack = read_challenge_ack_packet(stream)?;
    let expected_ack = challenge_digest(cookie, challenge);
    if ack != expected_ack {
        return Err(HandshakeError::DigestMismatch);
    }

    HandshakeResult::new(remote.name, remote.creation, remote.flags, negotiated_flags)
}

/// Runs the inbound OTP 23+ distribution handshake over an accepted stream.
pub fn respond_handshake<S: Read + Write>(
    stream: &mut S,
    local: &HandshakeNode,
    cookie: &str,
    challenge: u32,
) -> Result<HandshakeResult, HandshakeError> {
    let remote = read_name_packet(stream, false)?;
    let negotiated_flags = match DistributionFlags::negotiate(local.flags, remote.flags) {
        Ok(flags) => flags,
        Err(error) => {
            send_status_ignore_io_error(stream, "not_allowed");
            return Err(error);
        }
    };

    write_packet(stream, &encode_status("ok")?)?;
    write_packet(stream, &encode_name(local, Some(challenge))?)?;

    let reply = read_challenge_reply_packet(stream)?;
    let expected_digest = challenge_digest(cookie, challenge);
    if reply.digest != expected_digest {
        send_status_ignore_io_error(stream, "not_allowed");
        return Err(HandshakeError::DigestMismatch);
    }

    let ack_digest = challenge_digest(cookie, reply.challenge);
    write_packet(stream, &encode_challenge_ack(ack_digest))?;

    HandshakeResult::new(remote.name, remote.creation, remote.flags, negotiated_flags)
}

/// Computes the OTP distribution digest: MD5(cookie text concatenated with challenge text).
pub fn challenge_digest(cookie: &str, challenge: u32) -> [u8; DIGEST_LEN] {
    let input = format!("{cookie}{challenge}");
    md5::compute(input.as_bytes()).0
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NamePacket {
    name: String,
    creation: u32,
    flags: DistributionFlags,
    challenge: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChallengeReply {
    challenge: u32,
    digest: [u8; DIGEST_LEN],
}

fn write_packet<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), HandshakeError> {
    let length = u16::try_from(payload.len()).map_err(|_| {
        HandshakeError::MalformedPacket("handshake packet exceeds 16-bit length prefix".into())
    })?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

fn read_packet<R: Read>(reader: &mut R) -> Result<Vec<u8>, HandshakeError> {
    let mut length_bytes = [0_u8; 2];
    reader.read_exact(&mut length_bytes)?;
    let length = u16::from_be_bytes(length_bytes) as usize;
    if length == 0 {
        return Err(HandshakeError::MalformedPacket(
            "empty handshake packet".into(),
        ));
    }

    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

fn encode_name(node: &HandshakeNode, challenge: Option<u32>) -> Result<Vec<u8>, HandshakeError> {
    let name_bytes = node.name.as_bytes();
    let name_len = u16::try_from(name_bytes.len()).map_err(|_| {
        HandshakeError::MalformedPacket("node name exceeds 16-bit length field".into())
    })?;

    let mut payload =
        Vec::with_capacity(1 + 8 + 4 + challenge.map_or(0, |_| 4) + 2 + name_bytes.len());
    payload.push(TAG_NEW_NAME);
    payload.extend_from_slice(&node.flags.bits().to_be_bytes());
    if let Some(challenge) = challenge {
        payload.extend_from_slice(&challenge.to_be_bytes());
    }
    payload.extend_from_slice(&node.creation.to_be_bytes());
    payload.extend_from_slice(&name_len.to_be_bytes());
    payload.extend_from_slice(name_bytes);
    Ok(payload)
}

fn encode_status(status: &str) -> Result<Vec<u8>, HandshakeError> {
    if status.is_empty() {
        return Err(HandshakeError::MalformedPacket(
            "status must not be empty".into(),
        ));
    }
    let mut payload = Vec::with_capacity(1 + status.len());
    payload.push(TAG_STATUS);
    payload.extend_from_slice(status.as_bytes());
    Ok(payload)
}

fn encode_challenge_reply(challenge: u32, digest: [u8; DIGEST_LEN]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + 4 + DIGEST_LEN);
    payload.push(TAG_CHALLENGE_REPLY);
    payload.extend_from_slice(&challenge.to_be_bytes());
    payload.extend_from_slice(&digest);
    payload
}

fn encode_challenge_ack(digest: [u8; DIGEST_LEN]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + DIGEST_LEN);
    payload.push(TAG_CHALLENGE_ACK);
    payload.extend_from_slice(&digest);
    payload
}

fn read_status_packet<R: Read>(reader: &mut R) -> Result<String, HandshakeError> {
    let payload = read_packet(reader)?;
    require_tag(&payload, TAG_STATUS)?;
    let status = std::str::from_utf8(&payload[1..])
        .map_err(|_| HandshakeError::MalformedPacket("status is not valid UTF-8".into()))?;
    if status.is_empty() {
        return Err(HandshakeError::MalformedPacket("status is empty".into()));
    }
    Ok(status.to_owned())
}

fn read_name_packet<R: Read>(
    reader: &mut R,
    requires_challenge: bool,
) -> Result<NamePacket, HandshakeError> {
    let payload = read_packet(reader)?;
    if payload.first().copied() != Some(TAG_NEW_NAME) {
        let actual = payload.first().copied().ok_or_else(|| {
            HandshakeError::MalformedPacket("name packet was empty after framing".into())
        })?;
        return Err(HandshakeError::UnexpectedTag {
            expected: TAG_NEW_NAME,
            actual,
        });
    }

    parse_name_payload(&payload, requires_challenge)
}

fn read_challenge_reply_packet<R: Read>(reader: &mut R) -> Result<ChallengeReply, HandshakeError> {
    let payload = read_packet(reader)?;
    require_exact_len(&payload, 1 + 4 + DIGEST_LEN, "challenge reply")?;
    require_tag(&payload, TAG_CHALLENGE_REPLY)?;

    let challenge = u32::from_be_bytes(slice_to_array(&payload[1..5])?);
    let digest = slice_to_array(&payload[5..21])?;
    Ok(ChallengeReply { challenge, digest })
}

fn read_challenge_ack_packet<R: Read>(reader: &mut R) -> Result<[u8; DIGEST_LEN], HandshakeError> {
    let payload = read_packet(reader)?;
    require_exact_len(&payload, 1 + DIGEST_LEN, "challenge ack")?;
    require_tag(&payload, TAG_CHALLENGE_ACK)?;
    slice_to_array(&payload[1..17])
}

fn parse_name_payload(
    payload: &[u8],
    requires_challenge: bool,
) -> Result<NamePacket, HandshakeError> {
    if payload.len() < 1 + 8 + 4 + 2 {
        return Err(HandshakeError::MalformedPacket(
            "name packet too short for OTP 23+ fields".into(),
        ));
    }

    let flags = DistributionFlags::from_bits(u64::from_be_bytes(slice_to_array(&payload[1..9])?));

    if requires_challenge {
        parse_name_payload_with_challenge(payload, flags)
    } else {
        parse_name_payload_without_challenge(payload, flags)
    }
}

fn parse_name_payload_without_challenge(
    payload: &[u8],
    flags: DistributionFlags,
) -> Result<NamePacket, HandshakeError> {
    let creation = u32::from_be_bytes(slice_to_array(&payload[9..13])?);
    let name_len = u16::from_be_bytes(slice_to_array(&payload[13..15])?) as usize;
    let name_start = 15;
    let name_end = name_start + name_len;
    let name_bytes = payload.get(name_start..name_end).ok_or_else(|| {
        HandshakeError::MalformedPacket("name packet length exceeds payload".into())
    })?;
    let name = parse_name(name_bytes)?;
    Ok(NamePacket {
        name,
        creation,
        flags,
        challenge: None,
    })
}

fn parse_name_payload_with_challenge(
    payload: &[u8],
    flags: DistributionFlags,
) -> Result<NamePacket, HandshakeError> {
    if payload.len() < 1 + 8 + 4 + 4 + 2 {
        return Err(HandshakeError::MalformedPacket(
            "challenge name packet too short for OTP 23+ fields".into(),
        ));
    }
    let challenge = u32::from_be_bytes(slice_to_array(&payload[9..13])?);
    let creation = u32::from_be_bytes(slice_to_array(&payload[13..17])?);
    let name_len = u16::from_be_bytes(slice_to_array(&payload[17..19])?) as usize;
    let name_start = 19;
    let name_end = name_start + name_len;
    let name_bytes = payload.get(name_start..name_end).ok_or_else(|| {
        HandshakeError::MalformedPacket("challenge name packet length exceeds payload".into())
    })?;
    let name = parse_name(name_bytes)?;
    Ok(NamePacket {
        name,
        creation,
        flags,
        challenge: Some(challenge),
    })
}

fn parse_name(name_bytes: &[u8]) -> Result<String, HandshakeError> {
    let name = std::str::from_utf8(name_bytes)
        .map_err(|_| HandshakeError::MalformedPacket("node name is not valid UTF-8".into()))?;
    validate_name(name)?;
    Ok(name.to_owned())
}

fn validate_name(name: &str) -> Result<(), HandshakeError> {
    if name.is_empty() {
        return Err(HandshakeError::MalformedPacket(
            "node name must not be empty".into(),
        ));
    }
    if name.len() > u16::MAX as usize {
        return Err(HandshakeError::MalformedPacket(
            "node name exceeds 16-bit length field".into(),
        ));
    }
    Ok(())
}

fn require_tag(payload: &[u8], expected: u8) -> Result<(), HandshakeError> {
    match payload.first().copied() {
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => Err(HandshakeError::UnexpectedTag { expected, actual }),
        None => Err(HandshakeError::MalformedPacket(
            "empty handshake packet".into(),
        )),
    }
}

fn require_exact_len(payload: &[u8], expected: usize, context: &str) -> Result<(), HandshakeError> {
    if payload.len() == expected {
        Ok(())
    } else {
        Err(HandshakeError::MalformedPacket(format!(
            "{context} length was {}, expected {expected}",
            payload.len()
        )))
    }
}

fn slice_to_array<const N: usize>(slice: &[u8]) -> Result<[u8; N], HandshakeError> {
    <[u8; N]>::try_from(slice).map_err(|_| {
        HandshakeError::MalformedPacket(format!(
            "expected {N} bytes, received {} bytes",
            slice.len()
        ))
    })
}

fn is_success_status(status: &str) -> bool {
    status == "ok" || status == "ok_simultaneous"
}

fn send_status_ignore_io_error<W: Write>(writer: &mut W, status: &str) {
    if let Ok(payload) = encode_status(status) {
        let _ = write_packet(writer, &payload);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DistributionFlags, HandshakeError, HandshakeNode, challenge_digest, initiate_handshake,
        respond_handshake,
    };
    use std::collections::VecDeque;
    use std::io::{self, Read, Write};
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;

    const COOKIE: &str = "beam-cookie";
    const INITIATOR_CHALLENGE: u32 = 1_010_101;
    const RESPONDER_CHALLENGE: u32 = 2_020_202;

    #[test]
    fn complete_handshake_between_two_local_nodes() {
        let mut pair = MemoryStreamPair::new();
        let local = HandshakeNode::with_default_flags("left@localhost", 1)
            .expect("test node name should be valid");
        let remote = HandshakeNode::with_default_flags("right@localhost", 2)
            .expect("test node name should be valid");
        let responder_node = remote.clone();

        let responder = thread::spawn(move || {
            respond_handshake(
                &mut pair.right,
                &responder_node,
                COOKIE,
                RESPONDER_CHALLENGE,
            )
        });

        let initiator_result =
            initiate_handshake(&mut pair.left, &local, COOKIE, INITIATOR_CHALLENGE)
                .expect("initiator handshake should succeed");
        let responder_result = responder
            .join()
            .expect("responder thread should not panic")
            .expect("responder handshake should succeed");

        assert_eq!(initiator_result.remote_name(), remote.name());
        assert_eq!(initiator_result.remote_creation(), remote.creation());
        assert_eq!(responder_result.remote_name(), local.name());
        assert_eq!(responder_result.remote_creation(), local.creation());
        assert_eq!(
            initiator_result.negotiated_flags(),
            DistributionFlags::offered()
        );
        assert_eq!(
            responder_result.negotiated_flags(),
            DistributionFlags::offered()
        );
    }

    #[test]
    fn wrong_cookie_is_rejected() {
        let mut pair = MemoryStreamPair::new();
        let local = HandshakeNode::with_default_flags("left@localhost", 1)
            .expect("test node name should be valid");
        let remote = HandshakeNode::with_default_flags("right@localhost", 2)
            .expect("test node name should be valid");

        let responder = thread::spawn(move || {
            respond_handshake(
                &mut pair.right,
                &remote,
                "different-cookie",
                RESPONDER_CHALLENGE,
            )
        });

        let initiator_error =
            initiate_handshake(&mut pair.left, &local, COOKIE, INITIATOR_CHALLENGE)
                .expect_err("initiator should reject a bad challenge ack");
        let responder_error = responder
            .join()
            .expect("responder thread should not panic")
            .expect_err("responder should reject a bad digest");

        assert!(matches!(
            initiator_error,
            HandshakeError::MalformedPacket(_) | HandshakeError::UnexpectedTag { .. }
        ));
        assert_eq!(responder_error, HandshakeError::DigestMismatch);
    }

    #[test]
    fn flag_negotiation_intersects_capabilities() {
        let local = DistributionFlags::HANDSHAKE_23
            | DistributionFlags::PUBLISHED
            | DistributionFlags::UTF8_ATOMS;
        let remote = DistributionFlags::HANDSHAKE_23
            | DistributionFlags::UTF8_ATOMS
            | DistributionFlags::MAP_TAG;

        let negotiated = DistributionFlags::negotiate(local, remote)
            .expect("shared required flag should negotiate");

        assert_eq!(
            negotiated,
            DistributionFlags::HANDSHAKE_23 | DistributionFlags::UTF8_ATOMS
        );
    }

    #[test]
    fn missing_required_flag_is_rejected() {
        let local = DistributionFlags::PUBLISHED | DistributionFlags::UTF8_ATOMS;
        let remote = DistributionFlags::PUBLISHED | DistributionFlags::UTF8_ATOMS;

        let error = DistributionFlags::negotiate(local, remote)
            .expect_err("missing HANDSHAKE_23 must reject");

        assert!(matches!(error, HandshakeError::IncompatibleFlags { .. }));
    }

    #[test]
    fn digest_uses_cookie_text_concatenated_with_challenge_text() {
        let digest = challenge_digest("cookie", 12345);

        assert_eq!(digest, md5::compute(b"cookie12345").0);
    }

    #[test]
    fn malformed_packet_maps_to_handshake_error() {
        let mut stream = ReadOnlyStream::new(vec![0, 1, b'x']);
        let local = HandshakeNode::with_default_flags("left@localhost", 1)
            .expect("test node name should be valid");

        let error = respond_handshake(&mut stream, &local, COOKIE, RESPONDER_CHALLENGE)
            .expect_err("wrong tag should fail");

        assert_eq!(
            error,
            HandshakeError::UnexpectedTag {
                expected: b'N',
                actual: b'x'
            }
        );
    }

    #[test]
    fn bad_status_maps_to_handshake_error() {
        let mut stream = ReadOnlyStream::new(vec![0, 4, b's', b'n', b'o', b'k']);
        let local = HandshakeNode::with_default_flags("left@localhost", 1)
            .expect("test node name should be valid");

        let error = initiate_handshake(&mut stream, &local, COOKIE, INITIATOR_CHALLENGE)
            .expect_err("nok status should fail");

        assert_eq!(error, HandshakeError::BadStatus("nok".into()));
    }

    struct MemoryStreamPair {
        left: MemoryStream,
        right: MemoryStream,
    }

    impl MemoryStreamPair {
        fn new() -> Self {
            let left_to_right = Arc::new(Pipe::new());
            let right_to_left = Arc::new(Pipe::new());
            Self {
                left: MemoryStream {
                    input: Arc::clone(&right_to_left),
                    output: Arc::clone(&left_to_right),
                },
                right: MemoryStream {
                    input: left_to_right,
                    output: right_to_left,
                },
            }
        }
    }

    struct MemoryStream {
        input: Arc<Pipe>,
        output: Arc<Pipe>,
    }

    impl Read for MemoryStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for MemoryStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.output.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Pipe {
        state: Mutex<VecDeque<u8>>,
        available: Condvar,
    }

    impl Pipe {
        fn new() -> Self {
            Self {
                state: Mutex::new(VecDeque::new()),
                available: Condvar::new(),
            }
        }

        fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| io::Error::other("pipe mutex poisoned"))?;
            while state.is_empty() {
                state = self
                    .available
                    .wait(state)
                    .map_err(|_| io::Error::other("pipe mutex poisoned"))?;
            }

            let mut count = 0;
            while count < buffer.len() {
                match state.pop_front() {
                    Some(byte) => {
                        buffer[count] = byte;
                        count += 1;
                    }
                    None => break,
                }
            }
            Ok(count)
        }

        fn write(&self, buffer: &[u8]) -> io::Result<usize> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| io::Error::other("pipe mutex poisoned"))?;
            state.extend(buffer.iter().copied());
            self.available.notify_all();
            Ok(buffer.len())
        }
    }

    struct ReadOnlyStream {
        bytes: io::Cursor<Vec<u8>>,
        writes: Vec<u8>,
    }

    impl ReadOnlyStream {
        fn new(bytes: Vec<u8>) -> Self {
            Self {
                bytes: io::Cursor::new(bytes),
                writes: Vec::new(),
            }
        }
    }

    impl Read for ReadOnlyStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.bytes.read(buffer)
        }
    }

    impl Write for ReadOnlyStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.writes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
