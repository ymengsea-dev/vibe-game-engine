# Materials

A material is how a surface looks: its colour, how shiny or metallic it is, and the images painted on it. A model comes with its own material from the file you imported. You can change it on one object, or make a **material file** and use it on as many models as you like.

## Making a material file

In **Project Assets**, right-click the top folder or any folder and choose **New Material**. Studio makes `New Material.material` there and selects it. Rename it like any other file.

While a material file is selected in Project Assets, the Inspector shows a ball wearing it and its settings. Changes are saved to the file straight away, and every model using it updates.

| Setting | What it does |
| --- | --- |
| **Colour** | The base colour. With a Colour image, the image is tinted by it |
| **Metallic** | 0 for plastic, wood or stone; 1 for metal |
| **Roughness** | 0 is mirror-smooth and shiny; 1 is matte |
| **Glow** | Light the surface gives off by itself, unaffected by shadow |
| **Bumpiness** | How strongly the Bumps image bends the light, 0 to 2 |
| **Repeat** | How many times the images repeat across the surface, across and down |
| **Shift** | Moves the images along, in image widths |
| **See-through** | **Solid**; **Cut out**, where pixels whose alpha is below **Cut below** disappear (leaves, fences, grass); or **Blended**, see-through by alpha (glass, water; slower) |
| **Two-sided** | Draws the back of each face too |

### Images

Drag an image from Project Assets onto a slot. **Clear** takes it off.

| Slot | What the image is |
| --- | --- |
| **Colour** | The picture on the surface |
| **Bumps** | A normal map: small dents and bumps without extra geometry |
| **Metal / rough** | Roughness in the green channel and metallic in the blue, as glTF files store them. Multiplies the Roughness and Metallic settings |
| **Shadowing** | Ambient occlusion: darker in cracks, in the red channel |
| **Glow** | Where the surface glows, multiplied by Glow |

## Shader graphs

A shader graph builds the surface from nodes instead of the fixed settings above. In a material file's Inspector, set **Surface** to **Shader graph**: Studio starts a graph that does what the settings did (the Colour image times a colour, plus metallic and roughness) and opens the **Shader graph** window. **Edit graph** opens it again later. Setting **Surface** back to **Standard** removes the graph.

**Repeat**, **Shift**, **See-through**, **Two-sided** and the images stay in the Inspector and still apply. The graph's **Alpha** decides what **Cut out** and **Blended** hide.

Every graph has one **Surface** node. Its inputs are what you see:

| Input | What it does | Unwired |
| --- | --- | --- |
| **Base colour** | The colour | Light grey |
| **Metallic** | 0 plastic, 1 metal | 0 |
| **Roughness** | 0 shiny, 1 matte | 0.5 |
| **Normal** | Bends the light: 0, 0, 1 is flat. Wire a **Bumps image** here for bumps | Flat |
| **Glow** | Light the surface gives off | None |
| **Alpha** | See-through amount, used by **Cut out** and **Blended** | 1 |
| **Vertex offset** | Moves the surface's points, in metres: waving flags, bobbing water | None |

### Nodes

| Group | Nodes |
| --- | --- |
| Values | **Number**, **Colour**, **Vector**, **Parameter** |
| From the scene | **Time** (seconds), **UV** (where on the images), **World position**, **Normal**, **View direction** |
| Images | **Colour image**, **Bumps image**, **Metal / rough image**, **Shadowing image**, **Glow image**: read the material's image slots |
| Maths | **Add**, **Subtract**, **Multiply**, **Divide**, **Lerp**, **Clamp**, **Power**, **Sin**, **Cos**, **Dot**, **Normalise**, **Split**, **Combine**, **Step**, **Smoothstep**, **Remap** |
| Effects | **Fresnel** (bright rims), **Noise** (smooth random 0 to 1) |

Numbers and colours mix freely: a number wired into a colour input fills every channel, and maths nodes work on the widest thing wired into them. A wire can't loop back into a node that feeds it. When the graph can't be drawn, the window and the Inspector say why and outline the node at fault in red; objects keep the standard look until it is fixed.

### Parameters

A **Parameter** node is a named number or colour (up to seven per graph). Each object using the material lists them in its Inspector under **Material → Graph values**, where it can have its own value; **Reset** goes back to the graph's default. A script can change one while the game runs:

- Lua: `engine.set_material_value(object_id, "Glow", 2)` or `engine.set_material_value(object_id, "Tint", { r = 1, g = 0.5, b = 0 })`.
- Rust: `GameCommand::SetMaterialValue { object_id, name: "Glow".into(), value: [2.0; 4] }`.

A value set by a script lasts until the scene loads again.

## Using a material on a model

- Drag the material file from Project Assets onto a model in the Scene View, or onto its row in the Hierarchy.
- Or select the model, click **Customize material** in the Inspector's **Material** section, and drop the file on the **Material** slot.

The file replaces the model's own material completely, images included. The object's **Tint**, **Metallic** and **Roughness** still apply on top, so one material can look a little different on each object.

## Images on one object

In the object's **Material** section, the **Images** slots replace the material's own image for that object only, map by map. Empty slots keep the material's image. This works with the model's own material and with a material file.

**Use imported material** in the section's ⋯ menu removes every change, including the material file.

## Limits

- A material covers the whole model; a model can't have different materials on different parts.
- The ball preview shows the colour, shine and images. For a shader graph it shows the graph at time 0, without Vertex offset.
- Shader graphs draw on still models. Animated (skinned) models, foliage, many-copy groups and shadows use the standard look of the same file: an animated model with a graph material shows its Repeat, Shift and See-through but not the nodes.
- **Time** in the Scene View moves only when the view redraws (when something changes); in the game it moves smoothly.
- Images repeat; they cannot be set to stretch at the edges.
