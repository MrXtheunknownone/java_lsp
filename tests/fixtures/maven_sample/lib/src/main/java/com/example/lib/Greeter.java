package com.example.lib;

import com.google.gson.Gson;

public class Greeter {
    public String greet() {
        return new Gson().toJson("hello");
    }
}
