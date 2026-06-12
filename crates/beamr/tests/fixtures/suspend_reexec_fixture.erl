%% Fixture for the suspension re-execution regression tests.
%%
%% Mirrors the aion Gleam SDK shape that broke: a closure whose body
%% computes the suspending native's argument with a CROSS-MODULE call
%% before invoking the native (`fn() { ffi.sleep(duration.to_milliseconds(d)) }`).
%% The native parks via the message-wakeable `request_suspend`; a mailbox
%% marker wakes the process, which must re-execute from the suspension call
%% site (re-running the native, which then returns `{ok, Binary}`).
-module(suspend_reexec_fixture).
-export([run/1, run_precomputed/1]).

run(X) ->
    Thunk = fun() ->
        beamr_suspend_reexec_test:sleep(suspend_reexec_helper:to_ms(X))
    end,
    invoke(Thunk).

%% The aion SDK mitigation shape: the argument is precomputed outside the
%% closure, so the re-executed expression contains no cross-module call.
run_precomputed(X) ->
    Ms = suspend_reexec_helper:to_ms(X),
    Thunk = fun() -> beamr_suspend_reexec_test:sleep(Ms) end,
    invoke(Thunk).

invoke(Thunk) ->
    case Thunk() of
        {ok, Bin} when is_binary(Bin) -> {done, byte_size(Bin)};
        Other -> {unexpected, Other}
    end.
