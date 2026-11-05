resolve(fn x ->
result =
case :ok do
:a ->
fn ->
:ok
end

:a ->
fn ->
case :ok do
:a -> :ok
:a -> :ok
end
end

:a ->
fn ->
case :ok do
:a -> :ok
:a -> :ok
end
end

:a ->
fn ->
case :ok do
:a -> :ok
:a -> :ok
end
end

:a ->
case :ok do
"" -> :ok
end

:a ->
case :ok do
:a -> :ok
end

:a ->
:ok
end
end)
