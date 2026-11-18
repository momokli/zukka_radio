# Non-live layers.
# show and queue will become request.dynamic against the scheduler (M1).
# rotation and relay are static fallbacks.

rotation = playlist("/rotation/rotation.m3u", reload_mode="watch")

relay = input.http("https://ice1.somafm.com/groovesalad-128-mp3")

fun = single("/rotation/fun.mp3")
