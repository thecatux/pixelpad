-- Example PixelPad plugin: reports line/word/character counts for the
-- current buffer. Bound to Ctrl-W (not reserved by any built-in command).
--
-- Every plugin must define a global `plugin` table with at least a
-- `run` function; `name`, `description`, and `hotkey` are optional.
plugin = {
    name = "Word Count",
    description = "Shows line, word, and character counts for the buffer",
    hotkey = "ctrl-w",
}

function plugin.run()
    local lines = pixelpad:get_lines()
    local line_count = #lines
    local word_count = 0
    local char_count = 0

    for _, line in ipairs(lines) do
        char_count = char_count + #line
        for _ in line:gmatch("%S+") do
            word_count = word_count + 1
        end
    end

    pixelpad:message(string.format(
        "%d lines, %d words, %d chars", line_count, word_count, char_count
    ))
end
