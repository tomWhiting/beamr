%% Fixture exercising the OTP 24+ receive-marker optimisation end to end:
%% `recv_marker_reserve` / `recv_marker_bind` / `recv_marker_use` /
%% `recv_marker_clear` are emitted around a selective receive keyed on a
%% freshly created reference. `roundtrip/1` sends `{Ref, Msg}` to itself and
%% selectively receives the reply tagged with that same `Ref`, returning the
%% payload unchanged. This proves beamr decodes and executes the whole
%% recv_marker family (opcodes 173-176) with correct OTP arities.
%% Compile with `erlc recv_marker_fixture.erl` (OTP 24+) and commit the .beam
%% next to this source.
-module(recv_marker_fixture).
-export([roundtrip/1]).

roundtrip(Msg) ->
    Ref = make_ref(),
    self() ! {Ref, Msg},
    receive
        {Ref, Reply} -> Reply
    end.
