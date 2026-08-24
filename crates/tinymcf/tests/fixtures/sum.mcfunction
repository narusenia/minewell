# $sum = the total of storage test:mw items, by consuming a copy of the list.
# This is the lowering `for x in vec` uses: no macros, one list copy.
scoreboard players set $sum obj 0
data modify storage test:mw iter set from storage test:mw items
function test:sum_loop
