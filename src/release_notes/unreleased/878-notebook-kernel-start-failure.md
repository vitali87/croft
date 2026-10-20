fix: A notebook kernel that fails to start no longer wipes the cells' saved outputs or leaves them stuck at In [*], and the next run starts a new kernel, so installing ipykernel works without relaunching croft.
fix: Notebook: Restart Kernel starts a new kernel when the old one has stopped, instead of doing nothing.
