fix: After a port poll's thread crashes, croft keeps looking for ports on its usual cadence, instead of never retiring a stopped port or its forward again until a restart.
