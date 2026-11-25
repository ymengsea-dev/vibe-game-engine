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

## Using a material on a model

- Drag the material file from Project Assets onto a model in the Scene View, or onto its row in the Hierarchy.
- Or select the model, click **Customize material** in the Inspector's **Material** section, and drop the file on the **Material** slot.

The file replaces the model's own material completely, images included. The object's **Tint**, **Metallic** and **Roughness** still apply on top, so one material can look a little different on each object.

## Images on one object

In the object's **Material** section, the **Images** slots replace the material's own image for that object only, map by map. Empty slots keep the material's image. This works with the model's own material and with a material file.

**Use imported material** in the section's ⋯ menu removes every change, including the material file.

## Limits

- A material covers the whole model; a model can't have different materials on different parts.
- The ball preview shows the colour, shine and Colour image only.
- Images repeat; they cannot be set to stretch at the edges.
