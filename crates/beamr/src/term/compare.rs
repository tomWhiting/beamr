/// Term ordering and equality.
///
/// Implements both `==` semantics (number coercion: 1 == 1.0) and
/// `=:=` semantics (exact: 1 =/= 1.0). Term ordering follows the
/// BEAM order: number < atom < reference < fun < port < pid <
/// tuple < map < nil < list < binary. Structural comparison for
/// boxed terms recurses into elements.

pub(crate) fn _scaffold() {}
