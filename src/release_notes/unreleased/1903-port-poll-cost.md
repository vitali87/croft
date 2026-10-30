fix: The port poll behind the PORTS tab no longer runs `ss` every 3 seconds when lsof finds nothing listening in a terminal pane.
fix: With lsof older than 4.95 (Ubuntu 20.04 and 22.04) and a raised open-file limit, the port poll no longer keeps about a tenth of a CPU core busy while croft sits idle.
