use mlua::{Lua, MultiValue, Table, Value};

const REGISTRY_KEY: &str = "vge.vec3";

const VEC3_LUA: &str = r#"
local meta = {}
local methods = {}
meta.__index = methods

local function finite(n)
  return type(n) == "number" and n == n and n >= -3.4028234663852886e38
    and n <= 3.4028234663852886e38
end

local function make(x, y, z)
  if not (finite(x) and finite(y) and finite(z)) then
    error("vec3 components must be finite numbers", 3)
  end
  return setmetatable({ x = x, y = y, z = z }, meta)
end

local function isvec(v)
  return getmetatable(v) == meta
end

function meta.__add(a, b) return make(a.x + b.x, a.y + b.y, a.z + b.z) end
function meta.__sub(a, b) return make(a.x - b.x, a.y - b.y, a.z - b.z) end
function meta.__unm(a) return make(-a.x, -a.y, -a.z) end
function meta.__mul(a, b)
  if type(a) == "number" then return make(a * b.x, a * b.y, a * b.z) end
  if type(b) == "number" then return make(a.x * b, a.y * b, a.z * b) end
  return make(a.x * b.x, a.y * b.y, a.z * b.z)
end
function meta.__div(a, b) return make(a.x / b, a.y / b, a.z / b) end
function meta.__eq(a, b) return a.x == b.x and a.y == b.y and a.z == b.z end
function meta.__tostring(a)
  return string.format("vec3(%g, %g, %g)", a.x, a.y, a.z)
end

function methods.length(a) return math.sqrt(a.x * a.x + a.y * a.y + a.z * a.z) end
function methods.length_squared(a) return a.x * a.x + a.y * a.y + a.z * a.z end
function methods.normalized(a)
  local len = methods.length(a)
  if len == 0 then return make(0, 0, 0) end
  return make(a.x / len, a.y / len, a.z / len)
end
function methods.dot(a, b) return a.x * b.x + a.y * b.y + a.z * b.z end
function methods.cross(a, b)
  return make(a.y * b.z - a.z * b.y, a.z * b.x - a.x * b.z, a.x * b.y - a.y * b.x)
end
function methods.distance(a, b) return methods.length(a - b) end
function methods.lerp(a, b, t)
  return make(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t, a.z + (b.z - a.z) * t)
end

local constants = {
  zero = { 0, 0, 0 }, one = { 1, 1, 1 },
  up = { 0, 1, 0 }, down = { 0, -1, 0 },
  right = { 1, 0, 0 }, left = { -1, 0, 0 },
  forward = { 0, 0, -1 }, back = { 0, 0, 1 },
}

local vec3 = setmetatable({}, {
  __call = function(_, x, y, z)
    return make(x or 0, y or 0, z or 0)
  end,
  __index = function(_, key)
    local c = constants[key]
    if c then return make(c[1], c[2], c[3]) end
    return nil
  end,
})

return meta, vec3
"#;

/// Installs the global `vec3` and stores the shared metatable in the registry.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let (meta, vec3): (Table, Table) = lua.load(VEC3_LUA).set_name("=vec3").eval()?;
    lua.set_named_registry_value(REGISTRY_KEY, meta)?;
    lua.globals().set("vec3", vec3)
}

/// Builds a new vec3 table with the shared metatable.
pub(super) fn to_lua(lua: &Lua, v: glam::Vec3) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("x", f64::from(v.x))?;
    table.set("y", f64::from(v.y))?;
    table.set("z", f64::from(v.z))?;
    let meta: Table = lua.named_registry_value(REGISTRY_KEY)?;
    table.set_metatable(Some(meta))?;
    Ok(table)
}

/// Consumes a vec3/plain `{x, y, z}` table or three numbers from the front of `args`.
pub(super) fn vector_arg(args: &mut MultiValue, what: &str) -> mlua::Result<glam::Vec3> {
    let bad = || mlua::Error::RuntimeError(format!("{what} must be a vec3 or three numbers"));
    let [x, y, z] = match args.pop_front() {
        Some(Value::Table(table)) => {
            let mut parts = [0.0_f64; 3];
            for (part, key) in parts.iter_mut().zip(["x", "y", "z"]) {
                *part = match table.get::<Value>(key)? {
                    Value::Number(n) => n,
                    Value::Integer(n) => n as f64,
                    _ => return Err(bad()),
                };
            }
            parts
        }
        Some(first) => {
            let mut parts = [0.0_f64; 3];
            let mut next = Some(first);
            for part in parts.iter_mut() {
                *part = match next.take().or_else(|| args.pop_front()) {
                    Some(Value::Number(n)) => n,
                    Some(Value::Integer(n)) => n as f64,
                    _ => return Err(bad()),
                };
            }
            parts
        }
        None => return Err(bad()),
    };
    let v = glam::Vec3::new(x as f32, y as f32, z as f32);
    if !v.is_finite() {
        return Err(mlua::Error::RuntimeError(format!("{what} must be finite")));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use crate::{GameCommand, LuaRuntime};

    fn run(body: &str) -> Result<Vec<GameCommand>, crate::ScriptError> {
        let mut runtime = LuaRuntime::new().expect("runtime");
        runtime.set_scene_snapshot([("actor".to_owned(), None, Some([0.0, 0.0, 0.0]))]);
        runtime
            .load_script(
                "assets/scripts/main.lua",
                &format!("return {{ start = function() {body} end }}"),
            )
            .expect("script loads");
        runtime.start()
    }

    fn logs(body: &str) -> Vec<String> {
        run(body)
            .expect("start")
            .into_iter()
            .map(|c| match c {
                GameCommand::Log(m) => m,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn vec3_operators_and_methods() {
        let out = logs(
            "engine.log(tostring(vec3(1,2,3) + vec3(1,1,1) == vec3(2,3,4)))
             engine.log(tostring(vec3(1,2,3) * 2))
             engine.log(tostring(2 * vec3(1,2,3)))
             engine.log(tostring(vec3(1,2,3) * vec3(2,2,2)))
             engine.log(tostring(-vec3(1,0,0)))
             engine.log(string.format('%g', vec3(3,4,0):length()))
             engine.log(tostring(vec3(0,0,0):normalized()))
             engine.log(string.format('%g', vec3(1,0,0):dot(vec3(0,1,0))))
             engine.log(tostring(vec3(1,0,0):cross(vec3(0,1,0))))
             engine.log(string.format('%g', vec3(0,0,0):distance(vec3(0,3,4))))
             engine.log(tostring(vec3(0,0,0):lerp(vec3(10,0,0), 0.5)))
             engine.log(tostring(vec3()))
             engine.log(tostring(vec3(4,6,8) / 2))
             engine.log(tostring(vec3(1,2,3) - vec3(1,1,1)))
             engine.log(string.format('%g', vec3(3,4,0):length_squared()))
             engine.log(tostring(vec3(0,0,5):normalized()))",
        );
        assert_eq!(
            out,
            [
                "true",
                "vec3(2, 4, 6)",
                "vec3(2, 4, 6)",
                "vec3(2, 4, 6)",
                "vec3(-1, 0, 0)",
                "5",
                "vec3(0, 0, 0)",
                "0",
                "vec3(0, 0, 1)",
                "5",
                "vec3(5, 0, 0)",
                "vec3(0, 0, 0)",
                "vec3(2, 3, 4)",
                "vec3(0, 1, 2)",
                "25",
                "vec3(0, 0, 1)",
            ]
        );
    }

    #[test]
    fn vec3_constants_are_fresh_copies() {
        let out = logs(
            "local a = vec3.up a.y = 5 engine.log(tostring(vec3.up))
             engine.log(tostring(vec3.forward))",
        );
        assert_eq!(out, ["vec3(0, 1, 0)", "vec3(0, 0, -1)"]);
    }

    #[test]
    fn vec3_rejects_non_finite_and_bad_tables() {
        let err = run("local v = vec3(1, 0/0, 0)")
            .expect_err("nan")
            .to_string();
        assert!(err.contains("must be finite"), "{err}");
        for call in [
            "engine.set_position('actor', {x = 1})",
            "engine.set_position('actor', 'a')",
        ] {
            let err = run(call).expect_err("bad").to_string();
            assert!(
                err.contains("position must be a vec3 or three numbers"),
                "{err}"
            );
        }
    }

    #[test]
    fn vec3_rejects_values_outside_f32_range() {
        let err = run("local v = vec3(1e300, 0, 0)")
            .expect_err("big")
            .to_string();
        assert!(err.contains("must be finite"), "{err}");
    }

    #[test]
    fn vector_arg_accepts_tables_and_numbers_and_rejects_overflow() {
        use super::vector_arg;
        use mlua::{IntoLua, Lua, MultiValue, Value};
        let lua = Lua::new();
        let multi = |vals: Vec<Value>| MultiValue::from_iter(vals);
        let mut args = multi(vec![
            1.0.into_lua(&lua).unwrap(),
            2.into_lua(&lua).unwrap(),
            3.0.into_lua(&lua).unwrap(),
        ]);
        assert_eq!(
            vector_arg(&mut args, "position").unwrap(),
            glam::Vec3::new(1.0, 2.0, 3.0)
        );
        let table: mlua::Table = lua.load("return {x = 1, y = 2, z = 3}").eval().unwrap();
        let mut args = multi(vec![Value::Table(table)]);
        assert_eq!(
            vector_arg(&mut args, "position").unwrap(),
            glam::Vec3::new(1.0, 2.0, 3.0)
        );
        let mut args = multi(vec![
            1e300.into_lua(&lua).unwrap(),
            0.0.into_lua(&lua).unwrap(),
            0.0.into_lua(&lua).unwrap(),
        ]);
        let err = vector_arg(&mut args, "position").unwrap_err().to_string();
        assert!(err.contains("must be finite"), "{err}");
    }

    #[test]
    fn vec3_rejects_non_finite_constructor() {
        let err = run("local v = vec3(1, 0/0, 0)")
            .expect_err("nan")
            .to_string();
        assert!(err.contains("must be finite"), "{err}");
    }
}
