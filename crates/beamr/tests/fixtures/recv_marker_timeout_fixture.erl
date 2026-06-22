%% Fixture exercising the timeout path of the OTP 24+ receive-marker
%% optimisation: `await/0` reserves and binds a receive marker against a
%% freshly created reference, then selectively receives `{Ref, _}` with an
%% immediate (`after 0`) timeout. With an empty mailbox the after-clause
%% fires, so `recv_marker_clear` runs on the timeout path and the function
%% returns the `timeout` atom. This proves beamr drives `recv_marker_clear`
%% (opcode 174) correctly when a marked receive times out.
%% Compile with `erlc recv_marker_timeout_fixture.erl` (OTP 24+) and commit
%% the .beam next to this source.
-module(recv_marker_timeout_fixture).
-export([await/0]).

await() ->
    Ref = make_ref(),
    receive
        {Ref, Reply} -> Reply
    after 0 -> timeout
    end.
