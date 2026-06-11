%% Fixture exercising `receive ... after` timeout delivery through the
%% scheduler's receive-timer machinery: an empty-mailbox receive whose
%% after-clause must run, and a selective receive that is woken by a
%% non-matching message and must still time out on the original deadline.
%% Compile with `erlc recv_after_timer.erl` (OTP 25+) and commit the .beam
%% next to this source.
-module(recv_after_timer).
-export([plain_after/0, selective/0]).

%% Parks with only a timer: timer expiry must fall through to the
%% after-clause (the `timeout` instruction after wait_timeout).
plain_after() ->
    receive
        never -> matched
    after 50 -> timed_out
    end.

%% A non-matching message wakes the process mid-receive; the re-park must
%% keep the original timer armed (BEAM does not cancel the receive timer on
%% a message wakeup).
selective() ->
    receive
        match -> matched
    after 200 -> timed_out
    end.
