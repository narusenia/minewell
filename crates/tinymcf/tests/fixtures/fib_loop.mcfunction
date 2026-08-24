execute if score $n obj matches ..0 run return 0
scoreboard players operation $t obj = $a obj
scoreboard players operation $t obj += $b obj
scoreboard players operation $a obj = $b obj
scoreboard players operation $b obj = $t obj
scoreboard players remove $n obj 1
function test:fib_loop
