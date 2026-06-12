%% Cross-module argument helper for the re-execution regression fixture.
-module(suspend_reexec_helper).
-export([to_ms/1]).

to_ms(X) -> X * 1000.
