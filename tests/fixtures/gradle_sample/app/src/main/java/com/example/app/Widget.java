package com.example.app;

import lombok.Getter;
import lombok.Setter;

@Getter
@Setter
public class Widget {
    private String name;

    public String describe() {
        return "Widget: " + name;
    }
}
