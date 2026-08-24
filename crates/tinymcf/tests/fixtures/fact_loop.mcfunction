execute if score $n obj matches ..1 run return 0
scoreboard players operation $result obj *= $n obj
scoreboard players remove $n obj 1
function test:fact_loop
