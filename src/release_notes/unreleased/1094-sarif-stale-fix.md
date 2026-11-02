fix: A SARIF fix is refused, changing nothing, when its file changed since the analysis ran, instead of rewriting the wrong code (`y = 1` became `y = 1x is None`).
