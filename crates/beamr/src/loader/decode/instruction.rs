use super::compact::Operand;

#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    Label {
        label: u32,
    },
    FuncInfo {
        module: Operand,
        function: Operand,
        arity: Operand,
    },
    Move {
        source: Operand,
        destination: Operand,
    },
    Call {
        arity: Operand,
        label: Operand,
    },
    CallOnly {
        arity: Operand,
        label: Operand,
    },
    CallExt {
        arity: Operand,
        import: Operand,
    },
    CallExtOnly {
        arity: Operand,
        import: Operand,
    },
    Fmove {
        source: Operand,
        dest: Operand,
    },
    Fconv {
        source: Operand,
        dest: Operand,
    },
    Fadd {
        fail: Operand,
        left: Operand,
        right: Operand,
        dest: Operand,
    },
    Fsub {
        fail: Operand,
        left: Operand,
        right: Operand,
        dest: Operand,
    },
    Fmul {
        fail: Operand,
        left: Operand,
        right: Operand,
        dest: Operand,
    },
    Fdiv {
        fail: Operand,
        left: Operand,
        right: Operand,
        dest: Operand,
    },
    Fnegate {
        fail: Operand,
        source: Operand,
        dest: Operand,
    },
    CallLast {
        arity: Operand,
        label: Operand,
        deallocate: Operand,
    },
    CallExtLast {
        arity: Operand,
        import: Operand,
        deallocate: Operand,
    },
    Return,
    Allocate {
        stack_need: Operand,
        live: Operand,
    },
    AllocateHeap {
        stack_need: Operand,
        heap_need: Operand,
        live: Operand,
    },
    AllocateZero {
        stack_need: Operand,
        live: Operand,
    },
    Deallocate {
        words: Operand,
    },
    TestHeap {
        heap_need: Operand,
        live: Operand,
    },
    PutList {
        head: Operand,
        tail: Operand,
        destination: Operand,
    },
    PutTuple2 {
        destination: Operand,
        elements: Operand,
    },
    GetTupleElement {
        source: Operand,
        index: Operand,
        destination: Operand,
    },
    GetList {
        source: Operand,
        head: Operand,
        tail: Operand,
    },
    GetHd {
        source: Operand,
        destination: Operand,
    },
    GetTl {
        source: Operand,
        destination: Operand,
    },
    TypeTest {
        op: TypeTestOp,
        fail: Operand,
        value: Operand,
    },
    Comparison {
        op: ComparisonOp,
        fail: Operand,
        left: Operand,
        right: Operand,
    },
    TestArity {
        fail: Operand,
        tuple: Operand,
        arity: Operand,
    },
    IsTaggedTuple {
        fail: Operand,
        value: Operand,
        arity: Operand,
        tag: Operand,
    },
    SelectVal {
        value: Operand,
        fail: Operand,
        list: Operand,
    },
    SelectTupleArity {
        value: Operand,
        fail: Operand,
        list: Operand,
    },
    Jump {
        target: Operand,
    },
    Bif {
        op: BifOp,
        operands: Vec<Operand>,
    },
    Send,
    RemoveMessage,
    Timeout,
    LoopRec {
        fail: Operand,
        destination: Operand,
    },
    LoopRecEnd {
        fail: Operand,
    },
    Wait {
        fail: Operand,
    },
    WaitTimeout {
        fail: Operand,
        timeout: Operand,
    },
    Catch {
        destination: Operand,
        label: Operand,
    },
    CatchEnd {
        source: Operand,
    },
    Try {
        destination: Operand,
        label: Operand,
    },
    TryEnd {
        source: Operand,
    },
    TryCase {
        source: Operand,
    },
    TryCaseEnd {
        source: Operand,
    },
    BinaryOp {
        op: BinaryOp,
        operands: Vec<Operand>,
    },
    MapOp {
        op: MapOp,
        operands: Vec<Operand>,
    },
    MakeFun {
        operands: Vec<Operand>,
    },
    CallFun {
        arity: Operand,
    },
    CallFun2 {
        function: Operand,
        arity: Operand,
        destination: Operand,
    },
    Apply {
        arity: Operand,
    },
    ApplyLast {
        arity: Operand,
        deallocate: Operand,
    },
    Badmatch {
        value: Operand,
    },
    Badrecord {
        value: Operand,
    },
    CaseEnd {
        value: Operand,
    },
    IfEnd,
    Raise {
        stacktrace: Operand,
        reason: Operand,
    },
    RawRaise,
    Line {
        index: Operand,
    },
    Trim {
        words: Operand,
        remaining: Operand,
    },
    OnLoad,
    BuildStacktrace,
    Swap {
        left: Operand,
        right: Operand,
    },
    InitYregs {
        registers: Operand,
    },
    NifStart,
    UpdateRecord {
        operands: Vec<Operand>,
    },
    Generic {
        opcode: u8,
        name: &'static str,
        operands: Vec<Operand>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeTestOp {
    IsInteger,
    IsFloat,
    IsNumber,
    IsAtom,
    IsPid,
    IsReference,
    IsPort,
    IsNil,
    IsBinary,
    IsList,
    IsNonemptyList,
    IsTuple,
    IsFunction,
    IsBoolean,
    IsFunction2,
    IsBitstr,
    IsMap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComparisonOp {
    Lt,
    Ge,
    Eq,
    Ne,
    EqExact,
    NeExact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BifOp {
    Bif0,
    Bif1,
    Bif2,
    GcBif1,
    GcBif2,
    GcBif3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    BsGetInteger2,
    BsGetFloat2,
    BsGetBinary2,
    BsSkipBits2,
    BsTestTail2,
    BsTestUnit,
    BsMatchString,
    BsInitWritable,
    BsGetUtf8,
    BsSkipUtf8,
    BsGetUtf16,
    BsSkipUtf16,
    BsGetUtf32,
    BsSkipUtf32,
    BsGetTail,
    BsStartMatch3,
    BsGetPosition,
    BsSetPosition,
    BsStartMatch4,
    BsCreateBin,
    BsMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapOp {
    PutMapAssoc,
    PutMapExact,
    HasMapFields,
    GetMapElements,
}

pub(crate) fn instruction_opcode(instruction: &Instruction) -> Option<u8> {
    match instruction {
        Instruction::Label { .. } => Some(1),
        Instruction::FuncInfo { .. } => Some(2),
        Instruction::Call { .. } => Some(4),
        Instruction::CallLast { .. } => Some(5),
        Instruction::CallOnly { .. } => Some(6),
        Instruction::CallExt { .. } => Some(7),
        Instruction::CallExtLast { .. } => Some(8),
        Instruction::CallExtOnly { .. } => Some(78),
        Instruction::Fmove { .. } => Some(96),
        Instruction::Fconv { .. } => Some(97),
        Instruction::Fadd { .. } => Some(98),
        Instruction::Fsub { .. } => Some(99),
        Instruction::Fmul { .. } => Some(100),
        Instruction::Fdiv { .. } => Some(101),
        Instruction::Fnegate { .. } => Some(102),
        Instruction::IsTaggedTuple { .. } => Some(159),
        Instruction::Generic { opcode, .. } => Some(*opcode),
        _ => None,
    }
}
