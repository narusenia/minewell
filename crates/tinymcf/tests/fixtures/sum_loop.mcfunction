execute unless data storage test:mw iter[0] run return 0
execute store result score $x obj run data get storage test:mw iter[0]
scoreboard players operation $sum obj += $x obj
data remove storage test:mw iter[0]
function test:sum_loop
