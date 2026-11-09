# Pause Menu: Step by Step

Follow the steps in order. See the [overview](../README.md) for how the menu works. The images are in [assets/](../assets/).

## 1. Add the images

Choose **Assets → Import…** from the menu bar and pick the four `.png` files.

## 2. Create the objects

Create them in this order, because the Layout lists its items in the order they were made.

- **Top-level object:** right-click the empty space below the Hierarchy rows, then **Create**.
- **Object inside another:** right-click the parent's row, then **Create**.
- **Name:** select the row and press F2.

```
Pause Controller      Empty
Pause Menu            Game UI → Panel
└ Card                Game UI → Image
  └ Items             Game UI → Layout
    ├ Title Plate     Game UI → Image
    │ └ Title         Game UI → Label
    ├ Resume Art      Game UI → Image
    │ └ Resume        Game UI → Button
    ├ Restart Art     Game UI → Image
    │ └ Restart       Game UI → Button
    └ Hint            Game UI → Label
```

## 3. Set how each one looks

Select each row and set these in the Inspector, under **Game UI**. Leave everything else as it is. To set an image, drag it from Project Assets onto the **Image** slot.

| Object | Settings |
| --- | --- |
| Pause Menu | Size 10000 × 10000. Background `#2B1C17` at 50% opacity |
| Card | Image `pause_card.png`. Size 400 × 330 |
| Items | Anchor TopLeft. Offset 0, 0. Size 400 × 330. Gap 10. Padding 38 |
| Title Plate | Image `pause_title.png`. Size 200 × 64 |
| Title | Text `Paused`. Font size 26. Bold on. Anchor Left. Offset 26, 0. Size 160 × 40. Text color `#FFF3E6` |
| Resume Art | Image `button_primary.png`. Size 324 × 68 |
| Resume | Text `Resume`. Font size 18. Bold on. Size 324 × 68. Background at 0% opacity. Text color `#3A2822` |
| Restart Art | Image `button_secondary.png`. Size 324 × 68 |
| Restart | Text `Restart`. Font size 18. Bold on. Size 324 × 68. Background at 0% opacity. Text color `#3A2822` |
| Hint | Text `Press P to go on`. Font size 13. Size 324 × 24. Text color `#3A2822` |

Items inside the Layout show **Position: Set by the Layout**, so they have no Anchor or Offset to set.

Resume and Restart sit on top of their images with an invisible background. Your clicks land on the button, and you see the image behind it.

## 4. Add the rules

Select the object, then in **Visual Rules** click **Add rule** and pick the action from the first column. Then set **When**, and click **Add condition** if the table lists one.

**Pause Controller**

| Add rule | When | Condition | Settings |
| --- | --- | --- | --- |
| Set game state | Key pressed: P | `pause_open` = 0 | Set state: Paused |
| Set game state | Key pressed: P | `pause_open` = 1 | Set state: Playing |
| Enable or disable object | Game state changes: Paused | | Object: Pause Menu. Do: Enable |
| Change a variable | Game state changes: Paused | | Set `pause_open` to 1 |
| Enable or disable object | Game state changes: Playing | | Object: Pause Menu. Do: Disable |
| Change a variable | Game state changes: Playing | | Set `pause_open` to 0 |

**Resume**

| Add rule | When | Settings |
| --- | --- | --- |
| Set game state | This button is clicked | Set state: Playing |

**Restart**, in this order:

| Add rule | When | Settings |
| --- | --- | --- |
| Change a variable | This button is clicked | Set `pause_open` to 0 |
| Set game state | This button is clicked | Set state: Playing |
| Change scene | This button is clicked | Go to scene: your scene's file, such as `scenes/main.ron` |

Restart sets the variable itself because a new scene doesn't count the state it starts in as a change, so the Playing rules above would not run.

## 5. Hide it and test

1. Select **Pause Menu** and tick **Disabled**. It stays in the Scene View while it or anything inside it is selected, and disappears when you select something else.
2. Save the scene with ⌘S.
3. Press ⌘P to play, click the game, then press P.

To edit the menu later, select it or anything inside it in the Hierarchy. You don't need to untick **Disabled**.

## Checklist

| Do this | You should see |
| --- | --- |
| Press P | The menu opens, the game freezes and the mouse is free |
| Press P again | The menu closes and the game continues |
| Click Resume | The same |
| Click Restart | The scene starts over, playing, with the menu closed |
| Press P after Restart | The menu opens again |

## If something is wrong

| Problem | Check |
| --- | --- |
| The menu shows when the game starts | **Disabled** is ticked on Pause Menu |
| P does nothing | The first two rules are on Pause Controller, with the conditions set. Click the game so it has the keyboard |
| The menu opens but P won't close it | The `pause_open` rules exist and the name is spelled the same everywhere |
| A button doesn't click | The Button is inside its Art image, not beside it |
| Items overlap or are in the wrong order | They are direct children of Items. Drag rows in the Hierarchy to reorder |
| An image is a plain white box | The image was dropped on the Image slot, and the file is still in Project Assets |
| The mouse stays locked while paused | A script calls `engine.set_mouse_locked(true)` or similar. Remove it, or free the mouse in the script. Mouse cameras set up in Camera Follow free it on their own |

## Make it your own

- **Without art:** use a Panel for Card with Corner radius 18. Skip the Art images and put the Buttons straight in Items. Give them a Background color and Corner radius 10. The Buttons then also light up under the mouse, which image buttons can't do yet.
- **Another key:** change P in the first two rules. Avoid Esc, because it closes a game running in its own window.
