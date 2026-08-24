# $result = fib($n), with fib(0) = 0
scoreboard players set $a obj 0
scoreboard players set $b obj 1
function test:fib_loop
scoreboard players operation $result obj = $a obj
